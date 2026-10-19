use super::*;
use crate::{Host, SavedSelection, Wire};
use ion_ai::{
    BoxFuture, Content, ModelRequest, ModelResponse, ModelService, ModelStream, ModelStreamEvent,
    ProviderError, ResponseTermination, Role, Usage,
};
use ion_core::{AgentLimits, CodingAgent, CodingSession, ToolSet};
use std::{cell::Cell, fs, path::PathBuf, sync::atomic::AtomicUsize, time::Duration};
use tokio::sync::{Notify, Semaphore};

#[path = "preparation_tests.rs"]
mod preparation;
#[path = "snapshot_tests.rs"]
mod snapshots;

// Deliberately neither Clone, Deserialize, nor Sync: UI/protocol metadata is
// moved, never cloned into executors or model content.
#[derive(Serialize)]
struct Metadata {
    literal: String,
    marker: Cell<u8>,
}
fn meta(text: &str) -> Metadata {
    Metadata {
        literal: text.into(),
        marker: Cell::new(7),
    }
}
fn prompt(text: &str) -> UserInput {
    UserInput::Prompt {
        text: text.into(),
        images: Vec::new(),
    }
}

struct Model {
    requests: Mutex<Vec<ModelRequest>>,
    opened: Notify,
    permits: Semaphore,
    calls: AtomicUsize,
    burst: usize,
    panic: bool,
}
impl Model {
    fn new(burst: usize) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            opened: Notify::new(),
            permits: Semaphore::new(0),
            calls: AtomicUsize::new(0),
            burst,
            panic: false,
        })
    }
    async fn wait_calls(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let opened = self.opened.notified();
                if self.calls.load(Ordering::SeqCst) >= count {
                    break;
                }
                opened.await;
            }
        })
        .await
        .unwrap();
    }
    fn release(&self) {
        self.permits.add_permits(1);
    }
}
impl ModelService for Model {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.opened.notify_waiters();
            self.permits.acquire().await.unwrap().forget();
            assert!(!self.panic, "injected operation task panic");
            let text = "x".repeat(self.burst.max(1));
            let mut events = vec![ModelStreamEvent::TextDelta("x".into()); self.burst.max(1)];
            events.push(ModelStreamEvent::Completed(ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::Text(text)],
                    provider_replay: None,
                },
                usage: Usage::unknown(),
                termination: ResponseTermination::Completed,
                returned_model: None,
            }));
            Ok(Box::pin(futures_util::stream::iter(events.into_iter().map(Ok))) as ModelStream)
        })
    }
}

struct Fixture {
    root: PathBuf,
    host: Arc<Host>,
    selected: crate::Selection,
    control: SessionControl<Metadata>,
}
impl Fixture {
    fn new(model: Arc<Model>, images: bool) -> Self {
        let root = std::env::temp_dir().join(format!("ion-control-test-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let host = Arc::new(Host::new(root.join("config"), root.join("state")));
        let selected = host
            .models()
            .save_default(&SavedSelection {
                provider: "fixture".into(),
                model: "fixture".into(),
                endpoint: Some("http://127.0.0.1:1".into()),
                wire: Some(Wire::ChatCompletions),
                api_key_env: None,
                image_input: images,
            })
            .unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let mut setup = crate::CodingApplication::new(host.clone(), None)
            .setup(session, selected.clone(), None, None)
            .unwrap();
        setup.runtime.agent = Arc::new(
            CodingAgent::with_tool_set(
                model.clone(),
                Arc::new(ToolSet::new([])),
                setup.runtime.agent.model().clone(),
            )
            .with_limits(AgentLimits {
                image_input: images,
                ..AgentLimits::default()
            }),
        );
        Self {
            root,
            host,
            selected,
            control: setup.into_control(),
        }
    }
    fn fault(&self, predicate: &str) {
        rusqlite::Connection::open(self.control.session().path()).unwrap().execute_batch(&format!(
            "CREATE TRIGGER injected_fault BEFORE INSERT ON entries WHEN {predicate} BEGIN SELECT RAISE(ABORT, 'injected storage fault'); END;"
        )).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
async fn settle(
    control: &mut SessionControl<Metadata>,
) -> (Vec<ReturnedInput<Metadata>>, Settlement<Metadata>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut returned = Vec::new();
        loop {
            match control.next().await {
                ControlUpdate::Returned(input) => returned.push(input),
                ControlUpdate::Settled(settlement) => return (returned, settlement),
                ControlUpdate::Idle => panic!("expected settlement"),
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}
async fn wait_committed_end(control: &SessionControl<Metadata>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if control
                .session()
                .view()
                .unwrap()
                .entries
                .iter()
                .any(|entry| matches!(entry, ion_core::SessionEntry::TurnEnded { .. }))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn non_sync_metadata_does_not_prevent_sending_the_owner_or_its_waits() {
    fn require_send(_: impl Send) {}
    let mut fixture = Fixture::new(Model::new(1), false);
    require_send(fixture.control.next());
    require_send(fixture.control.shutdown());
    require_send(fixture.control.observe().next());
}

#[tokio::test]
async fn dispatched_prompt_is_not_prepared_again_before_acceptance() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    let id = fixture
        .control
        .submit(Delivery::Start, prompt("root"), meta("literal root"))
        .unwrap();
    assert!(
        matches!(fixture.control.next().await, ControlUpdate::Input { id: queued, .. } if queued == id)
    );
    model.wait_calls(1).await;
    let report = fixture.control.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
    let settlements = report
        .updates
        .into_iter()
        .filter_map(|update| match update {
            ControlUpdate::Settled(settlement) => Some(settlement),
            ControlUpdate::Returned(_) => panic!("accepted root cannot be returned"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(settlements.len(), 1);
    assert!(matches!(
        settlements[0].outcome,
        OperationOutcome::Turn(Err(CodingAgentError::Cancelled))
    ));
    assert_eq!(settlements[0].accepted[0].id, id);
    assert_eq!(settlements[0].accepted[0].metadata.literal, "literal root");
}

#[tokio::test]
async fn faults_before_and_after_acceptance_preserve_exact_custody() {
    for before in [true, false] {
        let model = Model::new(1);
        let mut fixture = Fixture::new(model.clone(), false);
        let predicate = if before {
            "json_extract(NEW.body, '$.kind') = 'turn_started'"
        } else {
            "json_extract(NEW.body, '$.kind') = 'assistant'"
        };
        fixture.fault(predicate);
        let message = Message::user_input("/literal is not a command\n".into(), []);
        let id = fixture
            .control
            .submit(
                Delivery::Start,
                UserInput::Message(message.clone()),
                meta("  /literal ORIGINAL\n"),
            )
            .unwrap();
        if !before {
            model.wait_calls(1).await;
            model.release();
        }
        let (returned, settlement) = settle(&mut fixture.control).await;
        assert!(matches!(settlement.outcome, OperationOutcome::Turn(Err(_))));
        if before {
            assert_eq!(returned.len(), 1);
            assert_eq!(returned[0].id, id);
            assert_eq!(returned[0].metadata.literal, "  /literal ORIGINAL\n");
            assert!(
                matches!(&returned[0].input, UserInput::Message(original) if original == &message)
            );
            assert!(settlement.accepted.is_empty());
            assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        } else {
            assert!(
                returned.is_empty(),
                "accepted inputs must never be replayable"
            );
            assert_eq!(settlement.accepted[0].id, id);
            assert_eq!(
                settlement.accepted[0].metadata.literal,
                "  /literal ORIGINAL\n"
            );
        }
        let report = fixture.control.shutdown().await;
        assert!(report.cleanup_errors.is_empty());
    }
}

#[tokio::test]
async fn acceptance_custody_precedes_progress_and_steering_uses_fifo_ids() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    let root = fixture
        .control
        .submit(Delivery::Start, prompt("same"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    let op = fixture.control.current_operation().unwrap();
    let first = fixture
        .control
        .submit(Delivery::Steer(op), prompt("same"), meta("first"))
        .unwrap();
    let second = fixture
        .control
        .submit(Delivery::Steer(op), prompt("same"), meta("second"))
        .unwrap();
    model.release();
    // Committed steering causes another model request, even with a final reply.
    model.wait_calls(2).await;
    {
        let custody = lock(&fixture.control.custody);
        for id in [root, first, second] {
            let input = &custody.inputs[&id];
            assert!(input.acceptance.is_some());
            assert!(input.original.is_none());
            assert!(input.inflight.is_none());
        }
        let ids = custody
            .updates
            .iter()
            .filter_map(|update| match update {
                ControlUpdate::Accepted { id, .. } => Some(*id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, [root, first, second]);
    }
    // Stop racing committed steering cannot turn it back into recoverable input.
    fixture.control.clear_queue();
    fixture.control.request_stop();
    let (returned, settlement) = settle(&mut fixture.control).await;
    assert!(returned.is_empty());
    assert_eq!(
        settlement
            .accepted
            .iter()
            .map(|input| input.metadata.literal.as_str())
            .collect::<Vec<_>>(),
        ["root", "first", "second"]
    );
}

#[tokio::test]
async fn stopped_success_does_not_dispatch_a_deliberate_followup() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture
        .control
        .submit(Delivery::Start, prompt("root"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    let next = fixture
        .control
        .submit(Delivery::FollowUp, prompt("later"), meta("literal later"))
        .unwrap();
    model.release();
    wait_committed_end(&fixture.control).await;
    fixture.control.request_stop();
    let (mut returned, settlement) = settle(&mut fixture.control).await;
    if let ControlUpdate::Returned(input) = fixture.control.next().await {
        returned.push(input);
    }
    assert!(matches!(settlement.outcome, OperationOutcome::Turn(Ok(_))));
    assert!(settlement.stop_requested);
    assert_eq!(returned.len(), 1);
    assert_eq!(returned[0].id, next);
    assert_eq!(returned[0].metadata.literal, "literal later");
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn lag_and_dropped_passive_observers_do_not_cancel_work() {
    let model = Model::new(OBSERVATION_CAPACITY * 4);
    let mut fixture = Fixture::new(model.clone(), false);
    let mut observer = fixture.control.observe();
    assert!(matches!(
        observer.next().await,
        Some(Observation::Gap { skipped: 0 })
    ));
    drop(fixture.control.observe());
    fixture
        .control
        .submit(Delivery::Start, prompt("root"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    model.release();
    wait_committed_end(&fixture.control).await;
    assert!(matches!(
        observer.next().await,
        Some(Observation::Gap { .. })
    ));
    // Never expose a surviving suffix as a complete provisional response.
    assert!(matches!(
        observer.next().await,
        Some(Observation::Event {
            event,
            ..
        }) if matches!(event.as_ref(), AgentEvent::AssistantCommitted { .. })
    ));
    let (returned, settlement) = settle(&mut fixture.control).await;
    assert!(returned.is_empty());
    assert!(matches!(settlement.outcome, OperationOutcome::Turn(Ok(_))));
    assert_eq!(settlement.accepted.len(), 1);
    assert!(matches!(fixture.control.next().await, ControlUpdate::Idle));
}

#[tokio::test]
async fn cancelled_shutdown_wait_retains_errors_recovery_and_closure() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture
        .control
        .submit(Delivery::Start, prompt("root"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    fixture
        .control
        .submit(Delivery::FollowUp, prompt("later"), meta("later"))
        .unwrap();
    fixture.fault("json_extract(NEW.body, '$.kind') = 'turn_ended'");
    // Poll exactly once; cancel the borrowed wait, not the retained owner.
    assert!(futures_util::poll!(Box::pin(fixture.control.shutdown())).is_pending());
    assert!(
        fixture
            .control
            .submit(Delivery::FollowUp, prompt("rejected"), meta("rejected"))
            .is_err()
    );
    let report = fixture.control.shutdown().await;
    let mut settled = 0;
    let mut returned = 0;
    for update in report.updates {
        match update {
            ControlUpdate::Settled(settlement) => {
                assert!(matches!(settlement.outcome, OperationOutcome::Turn(Err(_))));
                settled += 1;
            }
            ControlUpdate::Returned(input) => {
                assert_eq!(input.metadata.literal, "later");
                returned += 1;
            }
            _ => {}
        }
    }
    assert_eq!((settled, returned), (1, 1));
    assert!(report.cleanup_errors.is_empty());
}

#[tokio::test]
async fn queued_expansion_is_frozen_and_prepared_messages_are_literal() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fs::create_dir_all(fixture.root.join(".ion/prompts")).unwrap();
    let template = fixture.root.join(".ion/prompts/next.md");
    fs::write(&template, "OLD $1").unwrap();
    fixture.control.reload_resources().unwrap();
    fixture
        .control
        .submit(Delivery::Start, prompt("root"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    fixture
        .control
        .submit(
            Delivery::FollowUp,
            prompt("/next ARG"),
            meta("never model content"),
        )
        .unwrap();
    fixture
        .control
        .submit(
            Delivery::FollowUp,
            UserInput::Message(Message::user_input("/next literal".into(), [])),
            meta("also private"),
        )
        .unwrap();
    fs::write(&template, "NEW $1").unwrap();
    fixture.control.reload_resources().unwrap();
    model.release();
    settle(&mut fixture.control).await;
    model.wait_calls(2).await;
    assert_eq!(
        model.requests.lock().unwrap()[1].messages.last().unwrap(),
        &Message::user_input("OLD ARG".into(), [])
    );
    model.release();
    settle(&mut fixture.control).await;
    model.wait_calls(3).await;
    assert_eq!(
        model.requests.lock().unwrap()[2].messages.last().unwrap(),
        &Message::user_input("/next literal".into(), [])
    );
    model.release();
    settle(&mut fixture.control).await;
}

#[tokio::test]
async fn unread_return_capacity_is_reserved_before_more_effects() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture
        .control
        .submit(Delivery::Start, prompt("root"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    for _ in 1..MAX_ADMISSIONS {
        fixture
            .control
            .submit(Delivery::FollowUp, prompt("queued"), meta("queued"))
            .unwrap();
    }
    fixture.control.clear_queue();
    let rejected = fixture
        .control
        .submit(Delivery::FollowUp, prompt("kept"), meta("kept"))
        .unwrap_err();
    assert_eq!(rejected.metadata.literal, "kept");
    assert!(matches!(rejected.input, UserInput::Prompt { text, .. } if text == "kept"));
    assert!(rejected.reason.to_string().contains("capacity"));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    let mut returned = 0;
    while returned < MAX_ADMISSIONS - 1 {
        if let ControlUpdate::Returned(_) = fixture.control.next().await {
            returned += 1;
        }
    }
    assert!(
        fixture
            .control
            .submit(Delivery::FollowUp, prompt("newly admitted"), meta("new"))
            .is_ok()
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn withdraw_newest_followup_preserves_older_and_running_inputs() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture
        .control
        .submit(Delivery::Start, prompt("root"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    fixture
        .control
        .submit(Delivery::FollowUp, prompt("first"), meta("first literal"))
        .unwrap();
    let last = fixture
        .control
        .submit(Delivery::FollowUp, prompt("last"), meta("last literal"))
        .unwrap();
    assert_eq!(fixture.control.withdraw_last_follow_up(), Some(last));
    loop {
        if let ControlUpdate::Returned(input) = fixture.control.next().await {
            assert_eq!(input.id, last);
            assert_eq!(input.metadata.literal, "last literal");
            assert!(matches!(input.reason, ReturnReason::Cleared));
            break;
        }
    }
    model.release();
    settle(&mut fixture.control).await;
    model.wait_calls(2).await;
    assert_eq!(fixture.control.withdraw_last_follow_up(), None);
    model.release();
    let (returned, settlement) = settle(&mut fixture.control).await;
    assert!(returned.is_empty());
    assert_eq!(settlement.accepted[0].metadata.literal, "first literal");
}

#[tokio::test]
async fn ordinary_nonzero_shell_does_not_withdraw_explicit_followups() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture
        .control
        .shell("exit 7".into(), true, meta("shell"))
        .map_err(|(_, error)| error)
        .unwrap();
    fixture
        .control
        .submit(
            Delivery::FollowUp,
            prompt("deliberate next"),
            meta("followup"),
        )
        .unwrap();
    let (_, shell) = settle(&mut fixture.control).await;
    assert!(matches!(shell.outcome, OperationOutcome::Shell(Ok(output)) if output.is_error));
    assert_eq!(shell.metadata.unwrap().literal, "shell");
    model.wait_calls(1).await;
    model.release();
    let (returned, next) = settle(&mut fixture.control).await;
    assert!(returned.is_empty());
    assert_eq!(next.accepted[0].metadata.literal, "followup");
}

#[tokio::test]
async fn owner_drop_requests_stop_and_core_retains_the_actual_task() {
    let model = Model::new(1);
    let fixture = Fixture::new(model.clone(), false);
    let session = fixture.control.session().clone();
    let mut setup = crate::CodingApplication::new(fixture.host.clone(), None)
        .setup(session.clone(), fixture.selected.clone(), None, None)
        .unwrap();
    setup.runtime.agent = fixture.control.agent().clone();
    let mut control = setup.into_control::<Metadata>();
    control
        .submit(Delivery::Start, prompt("root"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    drop(control);
    session.shutdown().await.unwrap();
    assert!(session.view().unwrap().entries.iter().any(|entry| matches!(
        entry,
        ion_core::SessionEntry::TurnEnded {
            reason: ion_core::TurnEndReason::Cancelled,
            ..
        }
    )));
}

#[tokio::test]
async fn panic_blocks_reuse_and_preserves_primary_and_cleanup_failures() {
    let mut model = Model::new(1);
    Arc::get_mut(&mut model).unwrap().panic = true;
    let mut fixture = Fixture::new(model.clone(), false);
    fixture
        .control
        .submit(Delivery::Start, prompt("root"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    model.release();
    let (_, settlement) = settle(&mut fixture.control).await;
    assert!(matches!(
        settlement.outcome,
        OperationOutcome::Turn(Err(CodingAgentError::Session(
            CodingSessionError::Operation(ion_core::OperationError::TaskFailed(_))
        )))
    ));
    let rejected = fixture
        .control
        .submit(Delivery::Start, prompt("do not retry"), meta("kept"))
        .unwrap_err();
    assert_eq!(rejected.metadata.literal, "kept");
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert!(fixture.control.new_session().is_err());
    assert!(
        fixture
            .control
            .submit(
                Delivery::FollowUp,
                prompt("no panic retry"),
                meta("kept too")
            )
            .is_err()
    );
    let report = fixture.control.shutdown().await;
    assert!(matches!(
        report.cleanup_errors.as_slice(),
        [CodingSessionError::Operation(
            ion_core::OperationError::TaskFailed(_)
        )]
    ));
}
