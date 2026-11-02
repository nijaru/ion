use super::*;
use crate::{InputBudget, InputStatus, ToolSet};
use ion_ai::{Script, ScriptedModelService};
use std::cell::Cell;

#[tokio::test]
async fn root_receipt_precedes_observers_and_survives_observer_failure() {
    let root = std::env::temp_dir().join(format!("ion-input-receipt-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let session = Arc::new(Session::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let service = Arc::new(ScriptedModelService::new([]));
    let agent = Agent::with_tool_set(service.clone(), Arc::new(ToolSet::new([])), model);
    // Cell is Send but not Sync; admission must not strengthen that contract.
    let input = InputBudget::default()
        .prepare(user_text("once".into()), Cell::new(7), agent.limits())
        .unwrap();
    let receipt = input.receipt();
    let observed = receipt.clone();
    let session_id = session.id();
    let result = agent
        .submit_prepared(
            session.clone(),
            input,
            "test".into(),
            CancellationToken::new(),
            None::<Arc<SteeringInbox<()>>>,
            move |event| {
                if let AgentEvent::TurnAccepted { turn, entry, .. } = event {
                    assert_eq!(
                        observed.status(),
                        InputStatus::Committed(crate::InputCommit {
                            session: session_id,
                            turn,
                            entry,
                        })
                    );
                    panic!("lost passive observer");
                }
            },
        )
        .await;
    assert!(matches!(result, Err(AgentError::ObserverPanicked)));
    let InputStatus::Committed(commit) = receipt.status() else {
        panic!("lost admission")
    };
    assert_eq!(commit.session, session.id());
    assert!(service.requests().is_empty());
    let view = session.view().unwrap();
    assert!(matches!(
        &view.entries[(commit.entry - 1) as usize],
        crate::SessionEntry::TurnStarted { turn, input, .. }
            if *turn == commit.turn && input == &user_text("once".into())
    ));
    session.shutdown().await.unwrap();
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_intake_does_not_lock_producers_or_overtake_frozen_input() {
    use std::{
        sync::{
            Condvar,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    let root = std::env::temp_dir().join(format!("ion-intake-delay-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("session.sqlite");
    let session = Arc::new(Session::create(&path, Some(&root)).unwrap());
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let (turn, _) = session
        .begin_turn_message(user_text("root".into()), model)
        .unwrap();
    let budget = InputBudget::default();
    let inbox = Arc::new(SteeringInbox::new(AgentLimits::default(), budget.clone()));
    let input = budget
        .prepare(user_text("frozen".into()), (), AgentLimits::default())
        .unwrap();
    let receipt = input.receipt();
    inbox.push_prepared(input);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let timed_out = Arc::new(AtomicBool::new(false));
    let (entered, ready) = tokio::sync::oneshot::channel();
    let gate = release.clone();
    let timeout = timed_out.clone();
    let writer = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        entered.send(()).unwrap();
        let (_released, result) = gate
            .1
            .wait_timeout_while(gate.0.lock().unwrap(), Duration::from_secs(2), |released| {
                !*released
            })
            .unwrap();
        timeout.store(result.timed_out(), Ordering::SeqCst);
        connection.execute_batch("ROLLBACK").unwrap();
    });
    ready.await.unwrap();
    let lane = session.io().unwrap();
    let committing = inbox.clone();
    let work = tokio::spawn(async move {
        lane.run_with(move |session| {
            committing.record_pending(session, turn, AgentLimits::default())
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while receipt.status() == InputStatus::Pending {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    inbox.push("later".into()).unwrap();
    let stalled = timed_out.load(Ordering::SeqCst);
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    writer.join().unwrap();
    let committed = work.await.unwrap().unwrap();
    assert!(
        !stalled,
        "storage blocked the executor or synchronous producer until the OS deadline"
    );
    assert_eq!(committed.steering, vec![user_text("frozen".into())]);
    assert!(matches!(receipt.status(), InputStatus::Committed(_)));
    assert_eq!(
        inbox
            .take_uncommitted()
            .into_iter()
            .map(crate::PreparedInput::into_message)
            .collect::<Vec<_>>(),
        vec![user_text("later".into())]
    );
    session.shutdown().await.unwrap();
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn steering_receipts_remain_ordered_without_observer_delivery() {
    let root = std::env::temp_dir().join(format!("ion-steering-receipts-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let session = Arc::new(Session::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let service = Arc::new(ScriptedModelService::new([Script::Stream(vec![
        ion_ai::ModelStreamEvent::Completed(ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::Text("done".into())],
                provider_replay: None,
            },
            usage: ion_ai::Usage::unknown(),
            termination: ResponseTermination::Completed,
            returned_model: None,
        }),
    ])]));
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let agent = Agent::with_tool_set(service, Arc::new(ToolSet::new([])), model);
    let budget = InputBudget::default();
    let inbox = Arc::new(SteeringInbox::new(agent.limits(), budget.clone()));
    let mut receipts = Vec::new();
    for text in ["first", "second"] {
        let input = budget
            .prepare(user_text(text.into()), (), agent.limits())
            .unwrap();
        receipts.push(input.receipt());
        inbox.push_prepared(input);
    }
    agent
        .submit_with_steering(
            session.clone(),
            "root".into(),
            "test".into(),
            CancellationToken::new(),
            inbox.clone(),
            |_| {},
        )
        .await
        .unwrap();
    let committed = inbox.take_committed();
    assert_eq!(committed.len(), 2);
    let mut entries = Vec::new();
    for (acknowledgement, receipt) in committed.iter().zip(&receipts) {
        assert!(acknowledgement.receipt().same_input(receipt));
        let InputStatus::Committed(commit) = receipt.status() else {
            panic!("missing steering commit")
        };
        entries.push(commit.entry);
    }
    assert!(entries[0] < entries[1]);
    assert!(inbox.take_uncommitted().is_empty());
    session.shutdown().await.unwrap();
    drop(committed);
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}
