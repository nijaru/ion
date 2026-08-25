//! Reopen schema tests.

use super::support::*;

// ---- Reopen and schema integrity (Codex review blockers) ----

#[tokio::test]
async fn reopen_rebuilds_an_open_operation_and_blocks_new_work() {
    let db = temp_db("reopen");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("start"),
            ScriptedMessage::delayed(Duration::from_secs(30), "late"),
        ]),
        ToolRegistry::default(),
        store,
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }
    // The durable transcript at close time is whatever was committed;
    // a delta that never settled stays an ephemeral draft (§10.3).
    let before = session.snapshot().await.expect("snapshot");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    // Reopen the same session: the suspended operation settles as
    // cancelled (§9.5 — suspend is teardown with effects cancelled, so
    // it can never continue) and the session accepts new work.
    let store = SessionStore::open(&db).expect("reopen store");
    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let snapshot = session.snapshot().await.expect("snapshot");
    assert_eq!(
        snapshot.operation,
        OperationStatus::Idle,
        "a settled suspended operation must not block the session"
    );
    assert_eq!(
        snapshot.reopen_entry_count,
        Some(before.entries.len()),
        "the runtime owns the reopen boundary used by frontends"
    );
    // The transcript reproduced exactly what was committed before close.
    assert_eq!(
        entry_kinds(
            &snapshot
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| (i as u64, e.clone()))
                .collect::<Vec<_>>()
        ),
        entry_kinds(
            &before
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| (i as u64, e.clone()))
                .collect::<Vec<_>>()
        )
    );
    // The settlement is durable: the latest checkpoint is terminal.
    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(loaded.operations.len(), 1);
    assert!(matches!(
        loaded.operations[0].latest.1.state,
        OperationState::Finished(_) | OperationState::Suspended
    ));
    // New work is accepted immediately.
    session
        .submit_if_idle("new")
        .await
        .expect("submit after settle");
    collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[test]
fn store_refuses_a_database_from_a_newer_schema() {
    let db = temp_db("future");
    {
        let store = SessionStore::open(&db).expect("open");
        let _ = store;
    }
    // Simulate a future Ion bumping the schema version.
    let connection = rusqlite::Connection::open(&db).expect("raw open");
    connection
        .pragma_update(None, "user_version", 99)
        .expect("bump version");
    drop(connection);
    let err = SessionStore::open(&db).expect_err("foreign schema must be refused");
    assert!(err.to_string().contains("does not match"), "got: {err}");

    // A database from an older dev build is refused too: v0 migrates
    // nothing (no compatibility guarantees across builds).
    let connection = rusqlite::Connection::open(&db).expect("raw open");
    connection
        .pragma_update(None, "user_version", 2)
        .expect("stale version");
    drop(connection);
    let err = SessionStore::open(&db).expect_err("stale schema must be refused");
    assert!(err.to_string().contains("does not match"), "got: {err}");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn store_close_joins_writer_and_rejects_later_requests() {
    let store = SessionStore::open_in_memory().expect("store");
    store.close().await.expect("close store");
    assert!(matches!(
        store.latest_session().await,
        Err(crate::store::StoreError::Closed)
    ));
    store.close().await.expect("idempotent close");
}

#[tokio::test]
async fn settlement_must_match_a_pending_effect_of_the_operation() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // A settlement for an unknown or already-settled effect must be
    // rejected, not silently succeed on zero rows.
    let loaded = store.load(session_id).await.expect("load");
    let operation_id = loaded.operations[0].id;
    let ghost = crate::ids::EffectId::generate();
    let err = store
        .commit(CommitRequest {
            session_id,
            operation_id,
            checkpoint: CheckpointRecord {
                state_seq: 999,
                payload: CheckpointPayload {
                    state: OperationState::Finished(OperationOutcome::Completed),
                    cancel_requested: false,
                    prompt: String::new(),
                    capability_snapshot_id: loaded.operations[0].capability_snapshot.id.clone(),
                    open_effect: None,
                },
                capability_snapshot: loaded.operations[0].capability_snapshot.clone(),
            },
            entries: Vec::new(),
            open_effects: Vec::new(),
            settled_effects: vec![crate::store::SettledEffect {
                id: ghost,
                settlement: serde_json::json!({}),
            }],
            indeterminate_effects: Vec::new(),
            inbox: Vec::new(),
            inbox_applied: Vec::new(),
            usage: Vec::new(),
            context_manifests: Vec::new(),
            assistant_frames_delete: Vec::new(),
            tool_progress_delete: Vec::new(),
        })
        .await
        .expect_err("ghost settlement must fail");
    assert!(err.to_string().contains("matched no pending effect"));
}
