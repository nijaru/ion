//! Print mode tests.

use super::support::*;

// ---- Print-mode regression tests ----

#[tokio::test]
async fn streams_scripted_text_in_order() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("hel"),
            ScriptedMessage::text("lo"),
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (snapshot, mut events) = session.subscribe().await.expect("subscribe");
    assert_eq!(snapshot.operation, OperationStatus::Idle);

    let operation_id = session.submit_if_idle("hi").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert_eq!(
        kinds(&recorded),
        [
            "operation_started",
            "assistant_text_delta",
            "assistant_text_delta",
            "operation_finished"
        ]
    );
    assert!(matches!(
        recorded[0],
        RuntimeEvent::OperationStarted { operation_id: id, .. } if id == operation_id
    ));
    assert_eq!(texts(&recorded), vec!["hel".to_owned(), "lo".to_owned()]);
    let cursors: Vec<_> = recorded.iter().map(RuntimeEvent::cursor).collect();
    assert!(cursors.windows(2).all(|pair| pair[0] < pair[1]));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn cancel_stops_provider_before_later_chunks() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("one"),
            ScriptedMessage::delayed(Duration::from_secs(30), "two"),
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("slow").await.expect("submit");

    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("delta")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }

    session.cancel(operation_id).await.expect("cancel");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(recorded.iter().any(
        |event| matches!(event, RuntimeEvent::OperationCancelled { operation_id: id, .. } if *id == operation_id)
    ));
    assert!(!recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::AssistantTextDelta { text, .. } if text == "two"
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn busy_submit_is_rejected() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "later",
        )]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let first = session.submit_if_idle("a").await.expect("first submit");
    let err = session
        .submit_if_idle("b")
        .await
        .expect_err("second submit");
    assert_eq!(
        err,
        CommandError::Busy {
            operation_id: first
        }
    );
    session.cancel(first).await.expect("cancel");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn next_run_provisions_entry_before_later_operation_admission() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![
            ScriptedMessage::delayed(Duration::from_millis(100), "first"),
            ScriptedMessage::text("second"),
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let first = session.submit_if_idle("one").await.expect("first submit");
    sleep(STEP).await;
    let _queued_entry = session.next_run("two").await.expect("next run");

    let mut starts = Vec::new();
    let mut finishes = Vec::new();
    while finishes.len() < 2 {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        match event {
            RuntimeEvent::OperationStarted { operation_id, .. } => starts.push(operation_id),
            RuntimeEvent::OperationFinished { operation_id, .. } => finishes.push(operation_id),
            _ => {}
        }
    }
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0], first);
    assert_ne!(starts[1], first);
    assert_eq!(finishes, starts);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn pending_next_run_survives_close_and_promotes_after_reopen() {
    let db = temp_db("queued-reopen");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "active never settles",
        )]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let first = session.submit_if_idle("active").await.expect("submit");
    wait_for_state(&session, |state| {
        matches!(state, OperationState::AssistantEffectPending)
    })
    .await;
    let queued_entry = session.next_run("queued").await.expect("next run");

    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    let loaded = store.load(session_id).await.expect("load queued state");
    assert_eq!(loaded.operations.len(), 1);
    assert_eq!(loaded.operations[0].id, first);
    assert_eq!(
        loaded.operations[0].latest.1.state,
        OperationState::Suspended
    );
    let pending = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("main lane")
        .state
        .pending_next_run
        .as_ref()
        .expect("durable next run");
    assert_eq!(pending.entry_id, queued_entry);
    assert_eq!(pending.prompt, "queued");
    assert!(
        loaded.entries.iter().all(|entry| entry.id != queued_entry),
        "pending input must not exist as a semantic entry before acceptance"
    );

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_millis(100),
            "queued result",
        )]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let second = match snapshot.operation {
        OperationStatus::Active { operation_id, .. } => operation_id,
        OperationStatus::Idle => panic!("pending next run was not promoted on reopen"),
    };
    assert_ne!(first, second);
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::OperationFinished { operation_id, .. } if *operation_id == second
    )));

    let loaded = store.load(session_id).await.expect("load settled state");
    assert_eq!(loaded.operations.len(), 2);
    assert_eq!(loaded.operations[0].id, first);
    assert_eq!(loaded.operations[1].id, second);
    assert!(loaded.entries.iter().any(|entry| {
        entry.id == queued_entry
            && matches!(
                &entry.entry,
                SessionEntry::UserMessage { text } if text == "queued"
            )
    }));
    assert_eq!(
        loaded.operations[0].latest.1.state,
        OperationState::Finished(OperationOutcome::Cancelled)
    );
    assert_eq!(
        loaded.operations[1].latest.1.state,
        OperationState::Finished(OperationOutcome::Completed)
    );

    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn steer_requires_an_active_operation() {
    let runtime = tool_runtime();
    let session = runtime.session();
    assert_eq!(
        session.steer("nope").await,
        Err(CommandError::NoActiveOperation)
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn saturated_queue_returns_error() {
    let saturated = SaturatedHandle::new();
    let err = saturated.handle().close().await.expect_err("saturated");
    assert_eq!(err, CommandError::QueueSaturated);
}

#[tokio::test]
async fn close_rejects_new_work_and_joins() {
    let runtime = tool_runtime();
    let session = runtime.session();
    session.close().await.expect("close");
    let err = session.submit_if_idle("after").await.expect_err("closed");
    assert_eq!(err, CommandError::Closed);
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn delayed_chunk_respects_cancel_without_waiting_full_delay() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "late",
        )]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("wait").await.expect("submit");
    sleep(STEP).await;
    session.cancel(operation_id).await.expect("cancel");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationCancelled { .. })
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn session_returns_to_idle_after_finish_and_accepts_next_operation() {
    let runtime = start_runtime(ScriptedProvider::echo(), ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let first = session.submit_if_idle("a").await.expect("first");
    let _ = collect_until_terminal(&mut events).await.expect("first op");

    let snapshot = session.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.operation, OperationStatus::Idle);

    let second = session.submit_if_idle("b").await.expect("second");
    assert_ne!(first, second);
    let recorded = collect_until_terminal(&mut events)
        .await
        .expect("second op");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == second
    ));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn operation_state_is_one_replaceable_authoritative_row() {
    let db = temp_db("replaceable-operation-state");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("state me").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("settle");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);
    drop(store);

    let connection = rusqlite::Connection::open(&db).expect("open sqlite");
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM operation_state WHERE operation_id = ?1",
            [operation_id.as_uuid().to_string()],
            |row| row.get(0),
        )
        .expect("count operation state rows");
    assert_eq!(count, 1);
    let kind: String = connection
        .query_row(
            "SELECT kind FROM operation_state WHERE operation_id = ?1",
            [operation_id.as_uuid().to_string()],
            |row| row.get(0),
        )
        .expect("read operation state");
    assert_eq!(kind, "finished");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn new_runtime_persists_explicit_workspace_identity() {
    let db = temp_db("explicit-cwd");
    let store = SessionStore::open(&db).expect("open store");
    let workspace = std::env::temp_dir().join(format!("ion-runtime-cwd-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).expect("workspace");
    let runtime = Runtime::start_with_policy_and_resources_in_cwd(
        ScriptedProvider::echo(),
        ToolRegistry::with_cwd(&workspace),
        store.clone(),
        permissive_policy(),
        Vec::new(),
        workspace.to_string_lossy().into_owned(),
    );
    let session_id = runtime.session_id();
    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");

    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(loaded.session.cwd, workspace.to_string_lossy());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn reopened_runtime_gets_a_new_instance_identity() {
    let db = temp_db("runtime-instance-id");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let first = runtime
        .session()
        .snapshot()
        .await
        .expect("first snapshot")
        .runtime_instance_id;
    runtime
        .session()
        .close()
        .await
        .expect("close first runtime");
    runtime.join().await.expect("join first runtime");

    let reopened = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store,
        session_id,
    )
    .await
    .expect("reopen");
    let second = reopened
        .session()
        .snapshot()
        .await
        .expect("second snapshot")
        .runtime_instance_id;
    assert_ne!(first, second);
    reopened
        .session()
        .close()
        .await
        .expect("close reopened runtime");
    reopened.join().await.expect("join reopened runtime");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}
