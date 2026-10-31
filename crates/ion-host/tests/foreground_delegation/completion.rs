//! Actual transferred child -> explicitly authorized restricted peer, not parent routing.
use super::*;
use ion_core::{CompletionDelivery, CompletionStatus, SessionEntry};
use ion_host::BackgroundOwner;

#[tokio::test]
async fn joined_child_can_wake_a_peer_without_parent_input_or_coding_authority() {
    let owner = Arc::new(BackgroundOwner::new(1).unwrap());
    let mut source = Fixture::with_owner(Wait::Join, true, false, None, Some(owner.clone()), None);
    let peer_session = Arc::new(Session::create(source.root.join("peer.sqlite"), None).unwrap());
    let service = Arc::new(ScriptedModelService::new([response(vec![Content::Text(
        "peer ready".into(),
    )])]));
    let agent = Arc::new(Agent::with_tool_set(
        service.clone(),
        Arc::new(ToolSet::new(std::iter::empty())),
        model("peer"),
    ));
    let mut peer: SessionControl = SessionSetup::new(
        peer_session.clone(),
        SessionCatalog::new(source.root.clone(), None),
        Arc::new(App {
            agent,
            instructions: "restricted recipient",
            owner: None,
        }),
        None,
        None,
        None,
    )
    .unwrap()
    .with_completion_delivery(CompletionDelivery::Wake)
    .into_control();
    *source.delegate.completion.lock().unwrap() =
        Some(peer.completion_target(CompletionDelivery::Wake).unwrap());
    source.start();
    background::drain(&mut source.control).await;
    assert!(service.requests().is_empty());
    assert!(peer_session.view().unwrap().entries.is_empty());
    source.effect.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while owner.has_work() {
            owner.maintain().await;
        }
    })
    .await
    .unwrap();
    let snapshot = owner.snapshot();
    let run = &snapshot.runs[0];
    assert!(run.joined);
    assert_eq!(run.completion, Some(CompletionStatus::Queued));
    assert!(matches!(
        peer.next().await,
        ControlUpdate::Automated { turn: Some(1), .. }
    ));
    background::drain(&mut peer).await;
    let requests = service.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].tools.is_empty(),
        "receiving a notice must not inherit the producer's coding tools"
    );
    assert_eq!(source.effect.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        source.parent.requests().len(),
        2,
        "peer delivery must not wake the parent"
    );
    assert!(peer_session.view().unwrap().entries.iter().any(|entry| matches!(entry,
        SessionEntry::TurnStarted { completion, .. } if completion.len() == 1 && completion[0].source_session == source.session.id() && completion[0].run == run.receipt.record.run && completion[0].child_session == run.receipt.record.child
    )));
    assert_eq!(
        owner.snapshot().runs[0].completion,
        Some(CompletionStatus::Offered { turn: 1 })
    );
    assert!(
        owner.take(run.receipt.record.run).unwrap().is_ok(),
        "notification is not acknowledgement"
    );
    assert!(peer.shutdown().await.cleanup_errors.is_empty());
    assert!(source.control.shutdown().await.cleanup_errors.is_empty());
    owner.join_shutdown().await.unwrap();
}
