//! Store tests.

use super::support::*;

// ---- Durable session store (DESIGN.md §32 Step 2) ----

#[tokio::test]
async fn restart_reproduces_the_logical_transcript() {
    let db = temp_db("restart");
    let store = SessionStore::open(&db).expect("open store");
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool("bash", json!({"command":"echo persisted"})),
        ScriptedMessage::text("final\n"),
    ]);
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store);
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("read it").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    // Reopen the same database: the logical transcript must reproduce.
    let store = SessionStore::open(&db).expect("reopen store");
    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(
        entry_kinds(loaded.entries.iter().map(|record| &record.entry)),
        [
            "user_message",
            "assistant_message",
            "tool_call",
            "tool_result",
            "assistant_message",
        ]
    );
    assert_eq!(loaded.operations.len(), 1);
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(
        checkpoint.state,
        OperationState::Finished(OperationOutcome::Completed)
    );
    assert!(!checkpoint.cancel_requested);
    assert!(
        loaded
            .operations
            .iter()
            .all(|operation| operation.pending_inbox.is_empty())
    );
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn durable_admission_failure_is_visible_and_non_corrupting() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    // Wait until the session row is committed before injecting.
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    store.fail_next_write();

    let err = session
        .submit_if_idle("lost")
        .await
        .expect_err("submit must fail");
    assert!(matches!(err, CommandError::Persistence(_)));
    // No operation was installed: the session is still idle and usable.
    let snapshot = session.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.operation, OperationStatus::Idle);
    assert!(snapshot.entries.is_empty());

    let operation_id = session
        .submit_if_idle("kept")
        .await
        .expect("retry succeeds");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == operation_id
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn mid_operation_persistence_failure_fails_the_operation_visibly() {
    let store = SessionStore::open_in_memory().expect("store");
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool(
        "bash",
        json!({"command":"sleep 1 && echo slow"}),
    )]);
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store.clone());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    // Wait for the tool effect to start, then fail its settlement commit.
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::ToolStarted { .. }) {
            break;
        }
    }
    store.fail_next_write();

    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    let failed = recorded.iter().any(|event| {
        matches!(
            event,
            RuntimeEvent::OperationFailed { message, .. } if message.contains("persistence failed")
        )
    });
    assert!(failed, "persistence failure must be visible: {recorded:?}");
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. }))
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn cancel_request_is_durable() {
    let db = temp_db("cancel");
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
    let operation_id = session.submit_if_idle("slow").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }
    session.cancel(operation_id).await.expect("cancel");
    collect_until_terminal(&mut events).await.expect("settle");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    let store = SessionStore::open(&db).expect("reopen");
    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(
        checkpoint.cancel_requested,
        "the cancellation request must be durable"
    );
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn steer_is_durable_as_pending_inbox() {
    let db = temp_db("steer");
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
    session.steer("and also check tests").await.expect("steer");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    let store = SessionStore::open(&db).expect("reopen");
    let loaded = store.load(session_id).await.expect("load");
    let pending = &loaded.operations[0].pending_inbox;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, "and also check tests");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[test]
fn recovery_classes_match_the_design() {
    let registry = ToolRegistry::default();
    assert_eq!(registry.recovery_class("read"), RecoveryClass::ReplaySafe);
    assert_eq!(registry.recovery_class("search"), RecoveryClass::ReplaySafe);
    assert_eq!(registry.recovery_class("find"), RecoveryClass::ReplaySafe);
    assert_eq!(registry.recovery_class("write"), RecoveryClass::Reconcile);
    assert_eq!(registry.recovery_class("edit"), RecoveryClass::Reconcile);
    assert_eq!(registry.recovery_class("bash"), RecoveryClass::NeverReplay);
    assert_eq!(
        registry.recovery_class("unknown-tool"),
        RecoveryClass::NeverReplay
    );
}

#[test]
fn fail_operation_lands_from_any_open_state_and_never_from_finished() {
    for setup in [
        |m: &mut OperationMachine| {
            let _ = m.apply(Transition::StartModelStep {
                model: step_model(),
                plan: ContextPlan {
                    system: String::new(),
                    messages: Vec::new(),
                },
            });
        },
        |m: &mut OperationMachine| {
            let _ = m.apply(Transition::StartModelStep {
                model: step_model(),
                plan: ContextPlan {
                    system: String::new(),
                    messages: Vec::new(),
                },
            });
            let _ = m.apply(Transition::ProviderCompleted {
                text: String::new(),
                tool_calls: vec![call(1, "read")],
            });
        },
    ] {
        let (mut machine, _) = machine_with_tools("goal", vec![]);
        setup(&mut machine);
        let applied = machine
            .apply(Transition::FailOperation {
                message: "harness failure".to_owned(),
            })
            .expect("fail from an open state");
        assert_eq!(
            machine.state(),
            &OperationState::Finished(OperationOutcome::Failed("harness failure".to_owned()))
        );
        assert!(applied.cancel_effects);
    }
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine
        .apply(Transition::StartModelStep {
            model: step_model(),
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
    machine.apply(Transition::ProviderCancelled).expect("done");
    let err = machine
        .apply(Transition::FailOperation {
            message: "late".to_owned(),
        })
        .expect_err("finished is terminal");
    assert_eq!(err.transition, "fail_operation");
}

// ---- Schema gate: archive older dev stores, refuse newer ones (§33.12) ----

#[test]
fn older_schema_store_is_archived_and_reopened_fresh() {
    let db = temp_db("schema-archive");
    {
        // Simulate a database written by an older development build.
        let connection = rusqlite::Connection::open(&db).expect("open old store");
        connection
            .execute_batch("CREATE TABLE sessions (id TEXT);")
            .expect("create marker");
        connection
            .pragma_update(None, "user_version", 24)
            .expect("stamp version");
    }
    let store = SessionStore::open(&db).expect("older schema must archive, not refuse");
    let notice = store.startup_notice().expect("archive notice").to_owned();
    assert!(
        notice.contains("v24"),
        "notice names the old version: {notice}"
    );
    assert!(
        notice.contains("archived"),
        "notice says archived: {notice}"
    );

    // The original bytes survive untouched beside the fresh database.
    let parent = db.parent().unwrap();
    let backups: Vec<_> = std::fs::read_dir(parent)
        .expect("list data dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".v24.") && name.ends_with(".bak"))
        .collect();
    assert_eq!(backups.len(), 1, "exactly one v24 archive: {backups:?}");
    let archived = rusqlite::Connection::open(parent.join(&backups[0])).expect("open archive");
    let version: i64 = archived
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("archive version");
    assert_eq!(version, 24, "archived bytes keep the old version stamp");

    // The live database is usable at the current version.
    assert_eq!(store.startup_notice(), Some(notice.as_str()));
    drop(store);
    let reopened = SessionStore::open(&db).expect("reopen fresh store");
    assert!(reopened.startup_notice().is_none(), "no second archive");
}

#[test]
fn newer_schema_store_is_refused_visibly() {
    let db = temp_db("schema-newer");
    {
        let connection = rusqlite::Connection::open(&db).expect("open future store");
        connection
            .execute_batch("CREATE TABLE sessions (id TEXT);")
            .expect("create marker");
        connection
            .pragma_update(None, "user_version", 99)
            .expect("stamp future version");
    }
    let err = SessionStore::open(&db).expect_err("a newer database must be refused");
    let message = err.to_string();
    assert!(message.contains("newer"), "error is explicit: {message}");
    // Refusal must not touch the database.
    assert!(
        !db.parent()
            .unwrap()
            .join(format!("{}.v99", db.file_name().unwrap().to_string_lossy()))
            .exists()
    );
}

#[tokio::test]
async fn session_management_apis_list_rename_and_delete() {
    let store = SessionStore::open_in_memory().expect("store");
    let first = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_a = first.session();
    let (_snapshot, mut events) = session_a.subscribe().await.expect("subscribe");
    let op_a = session_a.submit_if_idle("hello").await.expect("submit a");
    collect_until_terminal(&mut events).await.expect("finish a");
    assert_eq!(op_a, op_a); // keep the operation id observable for the debug log
    session_a.close().await.expect("close a");
    first.join().await.expect("join a");

    let second = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_b = second.session();
    let (_snapshot, mut events_b) = session_b.subscribe().await.expect("subscribe b");
    let op_b = session_b.submit_if_idle("world").await.expect("submit b");
    collect_until_terminal(&mut events_b)
        .await
        .expect("finish b");
    let id_b = second.session_id();
    session_b.close().await.expect("close b");
    second.join().await.expect("join b");
    assert_ne!(op_a, op_b);

    // Listing shows roots, newest first, with entry counts.
    let listed = store.list_sessions().await.expect("list");
    assert_eq!(listed.len(), 2, "two roots: {listed:?}");
    assert_eq!(listed[0].id, id_b, "newest first");
    assert_eq!(listed[0].entry_count, 2, "user + assistant");
    assert_eq!(listed[1].entry_count, 2);

    // Rename is durable and trims; empty titles are refused.
    store
        .rename_session(id_b, "  deep dive  ")
        .await
        .expect("rename");
    let listed = store.list_sessions().await.expect("list");
    assert_eq!(listed[0].title, "deep dive");
    let err = store.rename_session(id_b, "   ").await.expect_err("empty");
    assert!(matches!(err, StoreError::InvalidTitle), "{err}");
    let err = store
        .rename_session(SessionId::generate(), "x")
        .await
        .expect_err("missing");
    assert!(matches!(err, StoreError::NotFound(_)), "{err}");

    // Delete removes the session; a second delete is a visible miss.
    store.delete_session(id_b).await.expect("delete");
    let listed = store.list_sessions().await.expect("list");
    assert_eq!(listed.len(), 1);
    let err = store
        .delete_session(id_b)
        .await
        .expect_err("already deleted");
    assert!(matches!(err, StoreError::NotFound(_)), "{err}");
}

#[tokio::test]
async fn clone_session_copies_history_with_lineage_and_a_clean_tip() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    let source_id = runtime.session_id();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let op = session.submit_if_idle("one").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("finish");
    assert_eq!(op, op); // keep the operation id observable for the debug log
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let target = store
        .clone_session(source_id, "fork of one")
        .await
        .expect("clone");

    let loaded = store.load(target).await.expect("load clone");
    assert_eq!(loaded.session.title, "fork of one");
    assert_eq!(loaded.session.fork_source_session_id, Some(source_id));
    assert_eq!(loaded.session.control_parent_session_id, None);
    assert_eq!(loaded.entries.len(), 2, "user + assistant copied");
    let main = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("main lane");
    assert!(main.state.current_operation.is_none());
    assert!(main.state.pending_next_run.is_none());
    assert!(main.state.leaf.is_some(), "clone keeps the tip");

    // The clone is a usable session with the copied history in place.
    let reopened = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
        target,
    )
    .await
    .expect("open clone");
    let handle = reopened.session();
    let (snapshot, _events) = handle.subscribe().await.expect("subscribe clone");
    assert_eq!(snapshot.entries.len(), 2, "copied history is visible");
    handle.close().await.expect("close clone");
    reopened.join().await.expect("join clone");

    // The source keeps its own tree and is unaffected.
    let source = store.load(source_id).await.expect("load source");
    assert_eq!(source.entries.len(), 2);
}

#[tokio::test]
async fn fork_before_copies_the_ancestor_path_and_returns_the_message() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    let source_id = runtime.session_id();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let _ = session.submit_if_idle("first").await.expect("submit one");
    collect_until_terminal(&mut events)
        .await
        .expect("finish one");
    let _ = session.submit_if_idle("second").await.expect("submit two");
    collect_until_terminal(&mut events)
        .await
        .expect("finish two");
    let _ = session.submit_if_idle("third").await.expect("submit three");
    collect_until_terminal(&mut events)
        .await
        .expect("finish three");
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Find the second user message's entry id.
    let loaded = store.load(source_id).await.expect("load source");
    let second_entry = loaded
        .entries
        .iter()
        .find(|record| {
            matches!(
                &record.entry,
                crate::SessionEntry::UserMessage { text } if text == "second"
            )
        })
        .expect("second user message exists");

    let (target, text) = store
        .fork_before(source_id, second_entry.id, "Fork of test")
        .await
        .expect("fork before");

    // The picked message's text returns for the composer (pi fills the
    // editor with the forked message).
    assert_eq!(text.as_deref(), Some("second"));

    let forked = store.load(target).await.expect("load fork");
    assert_eq!(forked.session.title, "Fork of test");
    assert_eq!(forked.session.fork_source_session_id, Some(source_id));
    assert_eq!(
        forked.session.fork_source_entry_id,
        Some(second_entry.id),
        "the fork records the picked entry"
    );
    // History: only the first turn's user+assistant pair precedes the
    // picked message — the fork cuts before "second".
    let texts: Vec<&str> = forked
        .entries
        .iter()
        .filter_map(|record| match &record.entry {
            crate::SessionEntry::UserMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["first"], "fork keeps only the prefix");
    assert_eq!(forked.entries.len(), 2, "user + assistant only: {forked:?}");
    let main = forked
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("main lane");
    assert!(main.state.current_operation.is_none());
    assert!(main.state.leaf.is_some(), "leaf pinned at the cut");

    // The forked session is usable and continues from the cut.
    let reopened = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
        target,
    )
    .await
    .expect("open fork");
    let handle = reopened.session();
    let (snapshot, mut events) = handle.subscribe().await.expect("subscribe fork");
    assert_eq!(snapshot.entries.len(), 2, "cut history is visible");
    let _ = handle.submit_if_idle("resumed").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("finish");
    handle.close().await.expect("close fork");
    reopened.join().await.expect("join fork");

    // The source is untouched.
    let source = store.load(source_id).await.expect("reload source");
    assert_eq!(source.entries.len(), 6, "three turns intact");
}

#[tokio::test]
async fn checkpoints_roundtrip_and_key_by_entry() {
    let store = SessionStore::open_in_memory().expect("store");
    let session_id = SessionId::generate();
    let record = crate::store::SessionRecord {
        id: session_id,
        cwd: "/tmp".into(),
        title: "checkpoints".into(),
        initial_model_ref: "ion_core::provider::ScriptedProvider".into(),
        control_parent_session_id: None,
        fork_source_session_id: None,
        fork_source_entry_id: None,
    };
    store.create_session(record).await.expect("create");
    // Direct entry insert is test-scoped: the store command exists for
    // fixture assembly (see AppendEntry).
    let entry = crate::store::EntryRecord::provision(
        1,
        crate::SessionEntry::UserMessage { text: "go".into() },
    );
    store
        .append_entry(session_id, "main", entry.clone())
        .await
        .expect("append");

    // No checkpoint yet.
    assert!(
        store
            .latest_checkpoint(session_id, entry.id)
            .await
            .expect("query empty")
            .is_none()
    );

    // Record and read back.
    store
        .record_checkpoint(session_id, entry.id, "abc123")
        .await
        .expect("record");
    let found = store
        .latest_checkpoint(session_id, entry.id)
        .await
        .expect("query")
        .expect("checkpoint found");
    assert_eq!(found.0, "abc123");

    // A different entry has no checkpoint (exact key, pi's map). The
    // entry chains after the lane's leaf, as every append does.
    let other = crate::store::EntryRecord::provision(
        2,
        crate::SessionEntry::UserMessage {
            text: "next".into(),
        },
    )
    .after(Some(entry.id));
    store
        .append_entry(session_id, "main", other.clone())
        .await
        .expect("append other");
    assert!(
        store
            .latest_checkpoint(session_id, other.id)
            .await
            .expect("query other")
            .is_none()
    );

    // Re-recording the same entry replaces the ref (one row per key).
    store
        .record_checkpoint(session_id, entry.id, "def456")
        .await
        .expect("re-record");
    let replaced = store
        .latest_checkpoint(session_id, entry.id)
        .await
        .expect("query replaced")
        .expect("still there");
    assert_eq!(replaced.0, "def456");
}

#[tokio::test]
async fn export_import_roundtrip_preserves_the_main_lane() {
    let store = SessionStore::open_in_memory().expect("store");
    let session_id = SessionId::generate();
    let record = crate::store::SessionRecord {
        id: session_id,
        cwd: "/tmp".into(),
        title: "export me".into(),
        initial_model_ref: "ion_core::provider::ScriptedProvider".into(),
        control_parent_session_id: None,
        fork_source_session_id: None,
        fork_source_entry_id: None,
    };
    store.create_session(record).await.expect("create");
    let user = crate::store::EntryRecord::provision(
        1,
        crate::SessionEntry::UserMessage { text: "go".into() },
    );
    store
        .append_entry(session_id, "main", user.clone())
        .await
        .expect("append user");
    let assistant = crate::store::EntryRecord::provision(
        2,
        crate::SessionEntry::AssistantMessage {
            text: "done".into(),
        },
    )
    .after(Some(user.id));
    store
        .append_entry(session_id, "main", assistant)
        .await
        .expect("append assistant");

    // Export: pi-grammar header plus parent-chained entry lines.
    let contents = store.export_session(session_id).await.expect("export");
    let lines: Vec<&str> = contents.trim_end().lines().collect();
    assert_eq!(lines.len(), 3, "header + two entries: {contents}");
    let header: crate::store::ExportedEntry = serde_json::from_str(lines[0]).expect("header line");
    assert_eq!(header.kind, "session");
    let session = header.session.as_ref().expect("ion session payload");
    assert_eq!(session.flavor, "ion");
    assert_eq!(session.cwd, "/tmp");
    let first: crate::store::ExportedEntry = serde_json::from_str(lines[1]).expect("entry line");
    assert_eq!(first.kind, "user_message");
    assert!(first.parent_id.is_none());
    let second: crate::store::ExportedEntry = serde_json::from_str(lines[2]).expect("entry line");
    assert_eq!(second.kind, "assistant_message");
    assert_eq!(second.parent_id.as_deref(), Some(first.id.as_str()));

    // Import into a fresh session: entries round-trip with fresh ids,
    // and the main lane points at the last one.
    let imported = store.import_session(contents).await.expect("import");
    assert_ne!(imported, session_id);
    let loaded = store.load(imported).await.expect("load imported");
    assert_eq!(loaded.entries.len(), 2);
    assert_eq!(
        loaded.entries[0].entry,
        crate::SessionEntry::UserMessage { text: "go".into() }
    );
    assert_eq!(
        loaded.entries[1].entry,
        crate::SessionEntry::AssistantMessage {
            text: "done".into()
        }
    );
    assert_eq!(loaded.entries[1].parent, Some(loaded.entries[0].id));
    let main = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == "main")
        .expect("main lane");
    assert_eq!(main.state.leaf, Some(loaded.entries[1].id));

    // A pi export (parses as a header but carries no ion session
    // payload) is refused cleanly.
    let pi_line = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"{}\",\"timestamp\":0,\"cwd\":\"/tmp\"}}",
        session_id
    );
    let err = store
        .import_session(pi_line)
        .await
        .expect_err("pi flavor refused");
    assert!(matches!(err, crate::store::ImportError::WrongFlavor));

    // Malformed JSON reports the line number.
    let err = store
        .import_session("not json\n".to_owned())
        .await
        .expect_err("malformed refused");
    assert!(matches!(
        err,
        crate::store::ImportError::Malformed { line: 1, .. }
    ));

    // Empty file.
    let err = store
        .import_session(String::new())
        .await
        .expect_err("empty refused");
    assert!(matches!(err, crate::store::ImportError::Empty));
}
