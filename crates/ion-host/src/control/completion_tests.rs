//! Real shared-control scheduling, distinct from producer/report acknowledgement.
use super::*;
use ion_core::{
    CompletionDelivery as Mode, CompletionNotice, CompletionRevocation, CompletionStatus,
};

fn notice() -> CompletionNotice {
    CompletionNotice {
        source_session: uuid::Uuid::now_v7(),
        run: uuid::Uuid::now_v7(),
        child_session: uuid::Uuid::now_v7(),
    }
}

#[tokio::test]
async fn authorized_peer_wakes_idle_without_fabricating_client_input_or_metadata() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    assert!(fixture.control.completion_target(Mode::Wake).is_err());
    fixture.control.set_completion_delivery(Mode::Wake).unwrap();
    let permit = fixture
        .control
        .completion_target(Mode::Wake)
        .unwrap()
        .reserve()
        .unwrap();
    fixture
        .control
        .submit(
            Delivery::Start,
            prompt("human"),
            meta("unsent literal owner"),
        )
        .unwrap();
    model.wait_calls(1).await;
    model.release();
    let (_, root) = settle(&mut fixture.control).await;
    assert!(!root.automated);
    assert_eq!(root.accepted[0].metadata.literal, "unsent literal owner");
    assert!(matches!(fixture.control.next().await, ControlUpdate::Idle));
    let data = notice();
    assert_ne!(
        data.source_session,
        fixture.control.session().id(),
        "peer, not a parent-only target"
    );
    permit.offer(data.clone());
    assert!(fixture.control.is_busy());
    let ControlUpdate::Automated {
        turn: Some(turn), ..
    } = fixture.control.next().await
    else {
        panic!("idle readiness must dispatch through shared control");
    };
    model.wait_calls(2).await;
    model.release();
    let (returned, result) = settle(&mut fixture.control).await;
    assert!(returned.is_empty());
    assert!(result.automated && result.accepted.is_empty() && result.metadata.is_none());
    assert_eq!(result.turn, Some(turn));
    assert!(matches!(result.outcome, OperationOutcome::Turn(Ok(_))));
    assert_eq!(
        permit.receipt().snapshot(),
        CompletionStatus::Offered { turn }
    );
    assert!(fixture.control.snapshot(|_| ()).unwrap().inputs.is_empty());
    assert_eq!(
        lock(&fixture.control.custody).occupied,
        0,
        "automatic settlement releases its own bounded capacity"
    );
    assert!(matches!(fixture.control.next().await, ControlUpdate::Idle));
    assert!(fixture.control.session().view().unwrap().entries.iter().any(|entry| matches!(entry, ion_core::SessionEntry::TurnStarted { completion, .. } if completion == &vec![data.clone()])));
}

#[tokio::test]
async fn unread_settlement_backpressures_wake_without_advertising_runnable_work() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture.control.set_completion_delivery(Mode::Wake).unwrap();
    let permit = fixture
        .control
        .completion_target(Mode::Wake)
        .unwrap()
        .reserve()
        .unwrap();
    fixture
        .control
        .submit(Delivery::Start, prompt("human"), meta("literal"))
        .unwrap();
    model.wait_calls(1).await;
    model.release();
    while fixture.control.is_busy() {
        fixture.control.maintain().await;
    }
    permit.offer(notice());
    assert!(
        !fixture.control.is_busy(),
        "unread reliable updates prevent maintenance from admitting a wake"
    );
    let mut settled = false;
    while let Some(update) = fixture.control.take_update() {
        if let ControlUpdate::Settled(result) = update {
            assert!(!result.automated);
            settled = true;
        }
    }
    assert!(
        settled,
        "passive maintenance must not consume the human settlement"
    );
    assert!(fixture.control.is_busy());
    assert!(matches!(
        fixture.control.next().await,
        ControlUpdate::Automated { .. }
    ));
    model.wait_calls(2).await;
    model.release();
    assert!(settle(&mut fixture.control).await.1.automated);
}

#[tokio::test]
async fn stop_and_binding_changes_permanently_revoke_old_targets_even_after_new_start() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture.control.set_completion_delivery(Mode::Wake).unwrap();
    let old_target = fixture.control.completion_target(Mode::Wake).unwrap();
    let old = old_target.reserve().unwrap();
    fixture.control.request_stop();
    fixture
        .control
        .submit(
            Delivery::Start,
            prompt("new explicit work"),
            meta("new literal"),
        )
        .unwrap();
    model.wait_calls(1).await;
    model.release();
    settle(&mut fixture.control).await;
    old.offer(notice());
    assert_eq!(
        old.receipt().snapshot(),
        CompletionStatus::Revoked {
            reason: CompletionRevocation::Stopped
        }
    );
    assert!(
        old_target.reserve().is_err(),
        "new Start must not rearm an old grant"
    );
    assert!(matches!(fixture.control.next().await, ControlUpdate::Idle));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);

    let target = fixture.control.completion_target(Mode::Wake).unwrap();
    let pending = target.reserve().unwrap();
    fixture.control.reload_resources().unwrap();
    pending.offer(notice());
    assert!(target.reserve().is_err());
    assert_eq!(
        pending.receipt().snapshot(),
        CompletionStatus::Revoked {
            reason: CompletionRevocation::BindingChanged
        }
    );
    assert!(matches!(fixture.control.next().await, ControlUpdate::Idle));
}

#[tokio::test]
async fn admitted_human_start_has_priority_and_acknowledgement_withdraws_queued_wakes() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture.control.set_completion_delivery(Mode::Wake).unwrap();
    let permit = fixture
        .control
        .completion_target(Mode::Wake)
        .unwrap()
        .reserve()
        .unwrap();
    permit.offer(notice());
    fixture
        .control
        .submit(
            Delivery::Start,
            prompt("human priority"),
            meta("exact literal"),
        )
        .unwrap();
    model.wait_calls(1).await;
    model.release();
    let (_, result) = settle(&mut fixture.control).await;
    assert!(!result.automated);
    assert_eq!(result.accepted[0].metadata.literal, "exact literal");
    assert!(matches!(fixture.control.next().await, ControlUpdate::Idle));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);

    let pending = fixture
        .control
        .completion_target(Mode::Wake)
        .unwrap()
        .reserve()
        .unwrap();
    pending.offer(notice());
    pending.acknowledge();
    assert!(matches!(fixture.control.next().await, ControlUpdate::Idle));
    assert_eq!(
        pending.receipt().snapshot(),
        CompletionStatus::Revoked {
            reason: CompletionRevocation::Acknowledged
        }
    );
}
