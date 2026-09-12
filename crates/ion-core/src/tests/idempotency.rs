//! Durable input-admission and caller-retry tests.

use super::support::*;
use crate::{CommandError, RequestKey};

#[tokio::test]
async fn request_key_recovers_original_receipt_after_lost_reply_and_reopen() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ModelExecution);
    let runtime = Runtime::start_with_effect_gate(
        ScriptedProvider::new(vec![ScriptedMessage::text("unreached before crash\n")]),
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let request_key = RequestKey::generate();

    // The model-execution gate is reached only after the operation/input
    // admission transaction committed. Drop the caller at that exact window:
    // accepted work must not depend on the response future surviving.
    let submit_session = session.clone();
    let submit = tokio::spawn(async move {
        submit_session
            .submit_with_key(request_key, "goal")
            .await
    });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("durable admission reached external-effect boundary");

    let loaded = store.load(session_id).await.expect("load admitted session");
    let original = loaded.operations.first().expect("accepted operation");
    let original_operation_id = original.id;
    let original_accepted_seq = original.accepted_seq;

    submit.abort();
    let _ = submit.await;
    runtime.crash();
    gate.release();
    drop(session);
    drop(runtime);

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("recovered\n")]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();

    let receipt = session
        .submit_with_key(request_key, "goal")
        .await
        .expect("same request key returns original receipt");
    assert_eq!(receipt.request_key, request_key);
    assert_eq!(receipt.operation_id, original_operation_id);
    assert_eq!(receipt.accepted_seq, original_accepted_seq);

    let changed_content = session
        .submit_with_key(request_key, "different goal")
        .await
        .expect_err("same key may not bind different content");
    assert!(matches!(
        changed_content,
        CommandError::IdempotencyConflict { request_key: key } if key == request_key
    ));

    session.create_lane("other").await.expect("other lane");
    let changed_target = session
        .submit_with_key_on_lane("other", request_key, "goal")
        .await
        .expect_err("same key may not bind another target");
    assert!(matches!(
        changed_target,
        CommandError::IdempotencyConflict { request_key: key } if key == request_key
    ));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn ordinary_submit_uses_the_same_receipt_backed_admission_path() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![ScriptedMessage::text("done\n")]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");

    let operation_id = session.submit_if_idle("goal").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == operation_id
    ));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
