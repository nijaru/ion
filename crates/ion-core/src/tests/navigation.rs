//! Navigation is an idle-lane transition, not a history rewrite.
use super::support::*;
use crate::EntryId;

#[tokio::test]
async fn navigation_retains_branches_and_survives_reopen() {
    let store = SessionStore::open_in_memory().unwrap();
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let id = runtime.session_id();
    let session = runtime.session();
    let (_, mut events) = session.subscribe().await.unwrap();
    session.submit_if_idle("first").await.unwrap();
    collect_until_terminal(&mut events).await.unwrap();
    let first = session.tree().await.unwrap().0.unwrap();
    session.submit_if_idle("second").await.unwrap();
    collect_until_terminal(&mut events).await.unwrap();
    let second = session.tree().await.unwrap().0.unwrap();
    let (_, mut observer) = session.subscribe().await.unwrap();
    let prefix = session.navigate_leaf(first).await.unwrap();
    let changed = observer.recv().await.unwrap();
    assert!(matches!(changed, RuntimeEvent::HistoryChanged { .. }));
    assert_eq!(changed.cursor(), prefix.cursor);
    assert_eq!(prefix.entries.len(), 2);
    session.submit_if_idle("alternate").await.unwrap();
    collect_until_terminal(&mut events).await.unwrap();
    let (alternate, tree) = session.tree().await.unwrap();
    assert_eq!(tree.len(), 6);
    assert_eq!(tree[4].parent, Some(first));
    let old_branch = session.navigate_leaf(second).await.unwrap();
    assert_eq!(old_branch.entries.len(), 4);
    assert!(
        old_branch
            .entries
            .iter()
            .any(|entry| matches!(entry, SessionEntry::UserMessage { text } if text == "second"))
    );
    assert!(
        !old_branch.entries.iter().any(
            |entry| matches!(entry, SessionEntry::UserMessage { text } if text == "alternate")
        )
    );
    session.close().await.unwrap();
    runtime.join().await.unwrap();
    let reopened =
        Runtime::open_session(ScriptedProvider::echo(), ToolRegistry::default(), store, id)
            .await
            .unwrap();
    let session = reopened.session();
    assert_eq!(session.tree().await.unwrap().0, Some(second));
    assert_eq!(session.tree().await.unwrap().1, tree);
    assert_eq!(
        session
            .navigate_leaf(alternate.unwrap())
            .await
            .unwrap()
            .entries
            .len(),
        4
    );
    session.close().await.unwrap();
    reopened.join().await.unwrap();
}

#[tokio::test]
async fn navigation_rejects_unknown_and_failed_writes_without_changing_leaf() {
    let store = SessionStore::open_in_memory().unwrap();
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    let (_, mut events) = session.subscribe().await.unwrap();
    session.submit_if_idle("first").await.unwrap();
    collect_until_terminal(&mut events).await.unwrap();
    let (leaf, entries) = session.tree().await.unwrap();
    let unknown = EntryId::generate();
    assert_eq!(
        session.navigate_leaf(unknown).await.unwrap_err(),
        CommandError::EntryNotFound(unknown)
    );
    store.fail_next_write();
    assert!(matches!(
        session.navigate_leaf(entries[0].id).await,
        Err(CommandError::Persistence(_))
    ));
    assert_eq!(session.tree().await.unwrap().0, leaf);
    session.close().await.unwrap();
    runtime.join().await.unwrap();
}

#[tokio::test]
async fn navigation_rejects_an_active_operation() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "later",
        )]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_, mut events) = session.subscribe().await.unwrap();
    let operation_id = session.submit_if_idle("busy").await.unwrap();
    let leaf = session.tree().await.unwrap().0.unwrap();
    assert_eq!(
        session.navigate_leaf(leaf).await.unwrap_err(),
        CommandError::Busy { operation_id }
    );
    session.cancel(operation_id).await.unwrap();
    collect_until_terminal(&mut events).await.unwrap();
    session.close().await.unwrap();
    runtime.join().await.unwrap();
}

async fn session_with_history(entries: Vec<SessionEntry>) -> (Runtime, SessionStore, Vec<EntryId>) {
    let store = SessionStore::open_in_memory().unwrap();
    let id = SessionId::generate();
    store
        .create_session(SessionRecord {
            id,
            cwd: "/tmp".into(),
            title: "navigation".into(),
            initial_model_ref: "scripted".into(),
            control_parent_session_id: None,
            fork_source_session_id: None,
            fork_source_entry_id: None,
        })
        .await
        .unwrap();
    let mut ids = Vec::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let record = EntryRecord::provision(index as u64 + 1, entry).after(ids.last().copied());
        ids.push(record.id);
        store.append_entry(id, "main", record).await.unwrap();
    }
    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
        id,
    )
    .await
    .unwrap();
    (runtime, store, ids)
}

fn call(call_id: u64) -> SessionEntry {
    SessionEntry::ToolCall {
        call: ToolCall {
            operation_id: OperationId::generate(),
            call_id,
            name: "read".into(),
            arguments: json!({"path": "file"}),
        },
    }
}

fn result(call_id: u64) -> SessionEntry {
    SessionEntry::ToolResult {
        result: ToolResult::Ok {
            call_id,
            output: "observed content".into(),
            artifact: None,
            images: Vec::new(),
        },
    }
}

#[tokio::test]
async fn navigation_rejects_single_and_partial_parallel_tool_exchanges_without_mutation() {
    for entries in [
        vec![call(1), result(1)],
        vec![call(1), call(2), result(1), result(2)],
    ] {
        let (runtime, store, ids) = session_with_history(entries).await;
        let session = runtime.session();
        let before = session.snapshot().await.unwrap();
        for &entry_id in &ids[..ids.len() - 1] {
            assert_eq!(
                session.navigate_leaf(entry_id).await.unwrap_err(),
                CommandError::IncompleteToolExchange(entry_id)
            );
            let after = session.snapshot().await.unwrap();
            assert_eq!(
                after.cursor, before.cursor,
                "rejection must not emit a history change"
            );
            assert_eq!(after.entries, before.entries);
            assert_eq!(
                store.load(runtime.session_id()).await.unwrap().lanes[0]
                    .state
                    .leaf,
                ids.last().copied()
            );
        }
        assert_eq!(
            session
                .navigate_leaf(*ids.last().unwrap())
                .await
                .unwrap()
                .entries,
            before.entries
        );
        session.close().await.unwrap();
        runtime.join().await.unwrap();
    }
}

#[tokio::test]
async fn navigation_uses_compaction_coverage_for_tool_exchange_boundaries() {
    let (runtime, _, ids) = session_with_history(vec![
        call(1),
        SessionEntry::Compaction {
            covers_through_seq: 0,
            summary: "not covering the call".into(),
        },
        SessionEntry::Compaction {
            covers_through_seq: 2,
            summary: "covering the earlier context".into(),
        },
    ])
    .await;
    let session = runtime.session();
    assert_eq!(
        session.navigate_leaf(ids[1]).await.unwrap_err(),
        CommandError::IncompleteToolExchange(ids[1])
    );
    session.navigate_leaf(ids[2]).await.unwrap();
    session.close().await.unwrap();
    runtime.join().await.unwrap();
}
