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
