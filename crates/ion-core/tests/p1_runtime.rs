mod p1_support;

use std::sync::Arc;
use std::time::Duration;

use p1_support::{
    AdmissionError, AdmissionMode, AgentStatus, AsyncTwoToolTurn, Behavior, BehaviorStep,
    PrototypeStore, Recovery, TurnCheckpoint, TwoToolTurn, UiState,
};
use tempfile::tempdir;
use tokio::sync::{Notify, Semaphore, oneshot};

#[test]
fn admission_retry_after_reopen_returns_original_receipt_and_rejects_rebinding() {
    let directory = tempdir().expect("tempdir");
    let db = directory.path().join("p1.sqlite");
    let first = {
        let mut store = PrototypeStore::open(&db).expect("store");
        store
            .admit("R1", store.root(), "hello", AdmissionMode::Prompt)
            .expect("first admission")
    };

    let mut reopened = PrototypeStore::open(&db).expect("reopen");
    let retry = reopened
        .admit("R1", reopened.root(), "hello", AdmissionMode::Prompt)
        .expect("idempotent retry");
    assert_eq!(retry, first);
    assert!(matches!(
        reopened.admit("R1", reopened.root(), "changed", AdmissionMode::Prompt),
        Err(AdmissionError::Conflict)
    ));
    assert!(matches!(
        reopened.admit("R1", reopened.root(), "hello", AdmissionMode::SpawnWorker),
        Err(AdmissionError::Conflict)
    ));
}

#[test]
fn reentrant_turn_separates_completion_order_from_model_projection_order() {
    let directory = tempdir().expect("tempdir");
    let db = directory.path().join("p1.sqlite");
    let mut store = PrototypeStore::open(&db).expect("store");
    let receipt = store
        .admit("turn", store.root(), "use tools", AdmissionMode::Prompt)
        .expect("admit");
    store.start_task(receipt.task_id).expect("start");

    let mut turn = TwoToolTurn::new();
    let specs = match turn.step(&store).expect("first step") {
        BehaviorStep::Effects(specs) => specs,
        other => panic!("unexpected step: {other:?}"),
    };
    let effects = store
        .open_effects(receipt.task_id, &specs)
        .expect("open effects");
    turn.bind_effects(effects.clone());
    store
        .set_checkpoint(receipt.task_id, turn.checkpoint())
        .expect("persist checkpoint");

    assert!(
        store
            .settle_effect(effects[1], "B-result")
            .expect("settle B")
    );
    assert!(matches!(
        turn.step(&store).expect("waiting step"),
        BehaviorStep::Wait
    ));
    assert!(
        store
            .settle_effect(effects[0], "A-result")
            .expect("settle A")
    );
    assert_eq!(
        store
            .settlement_order(receipt.task_id)
            .expect("settlement order"),
        vec!["B".to_owned(), "A".to_owned()]
    );
    assert_eq!(
        store
            .results_in_call_order(receipt.task_id)
            .expect("call order"),
        vec![
            ("A".to_owned(), "A-result".to_owned()),
            ("B".to_owned(), "B-result".to_owned()),
        ]
    );

    let checkpoint = store
        .checkpoint::<TurnCheckpoint>(receipt.task_id)
        .expect("checkpoint");
    drop(store);
    let reopened = PrototypeStore::open(&db).expect("reopen");
    let mut restored = TwoToolTurn::restore(checkpoint);
    assert_eq!(
        restored.step(&reopened).expect("resume"),
        BehaviorStep::Complete(vec!["A-result".to_owned(), "B-result".to_owned()])
    );
}

#[tokio::test]
async fn async_candidate_can_join_effects_but_has_no_durable_await_state() {
    let (first_tx, first_rx) = oneshot::channel();
    let (second_tx, second_rx) = oneshot::channel();
    let task = tokio::spawn(AsyncTwoToolTurn::run(first_rx, second_rx));
    second_tx.send("B".to_owned()).expect("send B");
    first_tx.send("A".to_owned()).expect("send A");
    assert_eq!(
        task.await.expect("join"),
        vec!["A".to_owned(), "B".to_owned()]
    );
}

#[test]
fn retained_worker_outlives_spawn_task_and_inspection_dispatches_nothing() {
    let directory = tempdir().expect("tempdir");
    let db = directory.path().join("p1.sqlite");
    let mut store = PrototypeStore::open(&db).expect("store");
    let receipt = store
        .admit("R2", store.root(), "worker job", AdmissionMode::SpawnWorker)
        .expect("spawn admission");
    let worker = receipt.created_agent.expect("worker id");
    let worker_task = receipt.worker_task.expect("worker task");

    assert_eq!(
        store.terminal(receipt.task_id).expect("spawn terminal"),
        Some("completed".to_owned())
    );
    assert_eq!(
        store.agent_status(worker).expect("worker status"),
        AgentStatus::Running
    );
    store.start_task(worker_task).expect("start worker");
    let before = store.effect_count().expect("effect count");
    let summary = store.group_summary().expect("group summary");
    let after = store.effect_count().expect("effect count");
    assert_eq!(before, after);
    assert!(summary.contains(&(worker, AgentStatus::Running)));
}

#[tokio::test]
async fn coordinator_wait_does_not_hold_the_only_model_permit() {
    let capacity = Arc::new(Semaphore::new(1));
    let completed = Arc::new(Notify::new());

    let wait = tokio::spawn({
        let completed = Arc::clone(&completed);
        async move {
            completed.notified().await;
        }
    });
    assert_eq!(capacity.available_permits(), 1);

    let worker = tokio::spawn({
        let capacity = Arc::clone(&capacity);
        let completed = Arc::clone(&completed);
        async move {
            let permit = capacity.acquire_owned().await.expect("model permit");
            completed.notify_one();
            drop(permit);
        }
    });

    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .expect("worker must not starve")
        .expect("worker task");
    tokio::time::timeout(Duration::from_secs(1), wait)
        .await
        .expect("wait completes")
        .expect("wait task");
}

#[test]
fn settlement_and_cancellation_are_generation_fenced_in_both_orders() {
    let directory = tempdir().expect("tempdir");
    let db = directory.path().join("p1.sqlite");
    let mut store = PrototypeStore::open(&db).expect("store");

    let first = store
        .admit("settle-first", store.root(), "one", AdmissionMode::Prompt)
        .expect("admit first");
    let first_generation = store.start_task(first.task_id).expect("start first");
    assert!(
        store
            .finish_task(first.task_id, first_generation, "completed")
            .expect("finish first")
    );
    assert_eq!(store.cancel_task(first.task_id).expect("late cancel"), None);
    assert_eq!(
        store.terminal(first.task_id).expect("first terminal"),
        Some("completed".to_owned())
    );

    let second = store
        .admit("cancel-first", store.root(), "two", AdmissionMode::Prompt)
        .expect("admit second");
    let old_generation = store.start_task(second.task_id).expect("start second");
    assert!(
        store
            .append_output(second.task_id, old_generation, "before cancel")
            .expect("early output")
    );
    let cancel_generation = store
        .cancel_task(second.task_id)
        .expect("cancel")
        .expect("cancel accepted");
    assert!(
        !store
            .append_output(second.task_id, old_generation, "late output")
            .expect("late output fenced")
    );
    assert!(
        !store
            .finish_task(second.task_id, old_generation, "completed")
            .expect("late completion fenced")
    );
    assert!(
        store
            .finish_cancel(second.task_id, cancel_generation)
            .expect("cancel cleanup")
    );
    assert_eq!(
        store.terminal(second.task_id).expect("second terminal"),
        Some("cancelled".to_owned())
    );
}

#[test]
fn focused_agent_ui_keeps_drafts_and_late_replies_bound_to_stable_targets() {
    let directory = tempdir().expect("tempdir");
    let db = directory.path().join("p1.sqlite");
    let mut store = PrototypeStore::open(&db).expect("store");
    let root = store.root();
    let receipt = store
        .admit("worker", root, "job", AdmissionMode::SpawnWorker)
        .expect("spawn");
    let worker = receipt.created_agent.expect("worker");

    let mut ui = UiState::new(root);
    ui.focus(worker);
    ui.set_draft("worker draft");
    ui.begin_command(7, worker);
    ui.focus(root);
    ui.set_draft("root draft");
    ui.apply_reply(7, "worker reply");

    assert_eq!(ui.draft(worker), "worker draft");
    assert_eq!(ui.draft(root), "root draft");
    assert_eq!(ui.replies, vec![(worker, "worker reply".to_owned())]);
    assert_eq!(store.path(), db.as_path());
}

#[test]
fn stale_effect_settlement_is_rejected_after_cancel_generation_advances() {
    let directory = tempdir().expect("tempdir");
    let db = directory.path().join("p1.sqlite");
    let mut store = PrototypeStore::open(&db).expect("store");
    let receipt = store
        .admit("effect-cancel", store.root(), "run", AdmissionMode::Prompt)
        .expect("admit");
    store.start_task(receipt.task_id).expect("start");
    let effects = store
        .open_effects(
            receipt.task_id,
            &[p1_support::EffectSpec {
                ordinal: 0,
                name: "read",
                recovery: Recovery::ReplaySafe,
            }],
        )
        .expect("open effect");
    store
        .cancel_task(receipt.task_id)
        .expect("cancel")
        .expect("cancel accepted");
    assert!(
        !store
            .settle_effect(effects[0], "late")
            .expect("late settlement fenced")
    );
}
