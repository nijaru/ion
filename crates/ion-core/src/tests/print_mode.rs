//! Print mode tests.

use super::support::*;
use crate::{ContextMessage, project};

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

#[tokio::test]
async fn dequeue_next_run_restores_the_prompt_and_leaves_nothing_queued() {
    let db = temp_db("dequeue-next-run");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "active never settles",
        )]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    let (snapshot, events) = session.subscribe().await.expect("subscribe");
    assert!(
        snapshot.pending_next_run.is_none(),
        "fresh session has no queued input"
    );
    session.submit_if_idle("active").await.expect("submit");
    wait_for_state(&session, |state| {
        matches!(state, OperationState::AssistantEffectPending)
    })
    .await;

    // Empty dequeue is a visible no-op.
    assert_eq!(
        session.dequeue_next_run().await.expect("empty dequeue"),
        None
    );

    let queued_entry = session.next_run("follow up").await.expect("next run");
    let snapshot = session.snapshot().await.expect("snapshot");
    let pending = snapshot
        .pending_next_run
        .as_ref()
        .expect("queued in snapshot");
    assert_eq!(pending.prompt, "follow up");
    assert_eq!(pending.entry_id, queued_entry);

    // Dequeue restores the prompt and clears the durable queue.
    assert_eq!(
        session.dequeue_next_run().await.expect("dequeue"),
        Some("follow up".to_owned())
    );
    let snapshot = session.snapshot().await.expect("snapshot after dequeue");
    assert!(snapshot.pending_next_run.is_none(), "queue is empty again");

    // Requeue still works after a dequeue: the lane accepts new input.
    let second_entry = session.next_run("requeued").await.expect("requeue");
    assert_ne!(second_entry, queued_entry);
    let snapshot = session.snapshot().await.expect("snapshot after requeue");
    assert_eq!(
        snapshot.pending_next_run.as_ref().expect("requeued").prompt,
        "requeued"
    );

    // Dequeued-then-requeued state must survive close/reopen untouched.
    let session_id = runtime.session_id();
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    // Reopen promotes the requeued prompt per the durable next-run
    // contract (§9.2): it becomes the recovered session's operation.
    let session = runtime.session();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = session.snapshot().await.expect("snapshot reopened");
        match &snapshot.operation {
            OperationStatus::Active { prompt, .. } if prompt == "requeued" => break,
            _ if Instant::now() < deadline => sleep(Duration::from_millis(20)).await,
            _ => panic!("requeued prompt never promoted after reopen"),
        }
    }
    session.close().await.expect("close reopened");
    runtime.join().await.expect("join reopened");
    let _ = events;
}

#[tokio::test]
async fn switch_thinking_is_durable_validated_and_applies_at_step_boundaries() {
    let db = temp_db("switch-thinking");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    let session_id = runtime.session_id();

    // Invalid levels are refused without touching durable state.
    let err = session
        .switch_thinking(Some("ultra".to_owned()))
        .await
        .expect_err("invalid level refused");
    assert!(err.to_string().contains("thinking"), "{err}");

    // Case-insensitive normalization: "High" persists as "high".
    let previous = session
        .switch_thinking(Some("High".to_owned()))
        .await
        .expect("switch");
    assert_eq!(previous, None);
    let snapshot = session.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.thinking.as_deref(), Some("high"));

    // Same-value switch is a no-op returning the current selection.
    let previous = session
        .switch_thinking(Some("high".to_owned()))
        .await
        .expect("idempotent switch");
    assert_eq!(previous, Some("high".to_owned()));

    // The frozen per-step ModelConfig carries the selection. Subscribe
    // before submitting: events between submit and subscribe are lost.
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let op = session.submit_if_idle("step").await.expect("submit");
    let _ = op;
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|event| matches!(event, RuntimeEvent::OperationFinished { .. })),
        "operation completed"
    );

    // Clearing restores the adapter default (None) durably.
    let previous = session.switch_thinking(None).await.expect("clear");
    assert_eq!(previous, Some("high".to_owned()));
    let snapshot = session.snapshot().await.expect("snapshot after clear");
    assert_eq!(snapshot.thinking, None);

    // The selection survives close/reopen from the durable lane config.
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let previous = session
        .switch_thinking(Some("low".to_owned()))
        .await
        .expect("switch after reopen");
    assert_eq!(
        previous, None,
        "reopen restored the cleared default before this switch"
    );
    session.close().await.expect("close reopened");
    runtime.join().await.expect("join reopened");
}

/// Poll until one shell passthrough command settles durably.
async fn wait_for_entry(session: &SessionHandle, command: &str) {
    for _ in 0..100 {
        let snapshot = session.snapshot().await.expect("snapshot");
        if snapshot.entries.iter().any(|entry| {
            matches!(
                entry,
                SessionEntry::ShellExecution { command: c, .. } if c == command
            )
        }) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("shell entry for {command:?} never settled");
}

#[tokio::test]
async fn shell_passthrough_settles_durably_projects_and_excludes() {
    let db = temp_db("shell-passthrough");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            std::time::Duration::from_secs(30),
            "late",
        )]),
        ToolRegistry::with_cwd(std::env::temp_dir()),
        store.clone(),
    );
    let session = runtime.session();
    let session_id = runtime.session_id();

    // `!echo`: Ok returns at intent commit; settlement follows and is
    // durable on the branch. Poll the snapshot for the settled entry.
    session
        .run_shell("printf ion-shell-test", false)
        .await
        .expect("shell passthrough accepted");
    wait_for_entry(&session, "printf ion-shell-test").await;

    // `!!echo` settles excluded.
    session
        .run_shell("printf ion-hidden", true)
        .await
        .expect("hidden passthrough accepted");
    wait_for_entry(&session, "printf ion-hidden").await;

    // Both entries are durable on the main branch, newest last.
    let snapshot = session.snapshot().await.expect("snapshot");
    let shell_entries: Vec<_> = snapshot
        .entries
        .iter()
        .filter(|entry| matches!(entry, SessionEntry::ShellExecution { .. }))
        .collect();
    assert_eq!(shell_entries.len(), 2, "both shell runs are durable");
    let (first, second) = (&shell_entries[0], &shell_entries[1]);
    match (first, second) {
        (
            SessionEntry::ShellExecution {
                command,
                output,
                exclude_from_context: false,
                ..
            },
            SessionEntry::ShellExecution {
                exclude_from_context: true,
                ..
            },
        ) => {
            assert_eq!(command, "printf ion-shell-test");
            assert!(
                output.contains("ion-shell-test"),
                "output settled: {output}"
            );
        }
        other => panic!("unexpected shell entries: {other:?}"),
    }

    // A visible shell entry joins the model projection in pi's shape; an
    // excluded one never does.
    let plan = project(&snapshot.entries, 1);
    let user_texts: Vec<String> = plan
        .messages
        .iter()
        .map(|message| match message {
            ContextMessage::User { content } => content.clone(),
            other => panic!("shell projection must be user content: {other:?}"),
        })
        .collect();
    assert!(
        user_texts
            .iter()
            .any(|text| text.contains("Ran `printf ion-shell-test`")),
        "visible shell run joined the projection: {user_texts:?}"
    );
    assert!(
        !user_texts.iter().any(|text| text.contains("ion-hidden")),
        "excluded shell run never joins the projection: {user_texts:?}"
    );

    // Busy refusal: a running operation owns the branch. The delayed
    // message keeps the operation active across the shell attempt.
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let op = session
        .submit_if_idle("hold")
        .await
        .expect("submit while idle");
    let err = session
        .run_shell("true", false)
        .await
        .expect_err("busy lane refuses shell");
    assert!(
        matches!(err, CommandError::Busy { .. }),
        "expected busy, got {err:?}"
    );
    session.cancel(op).await.expect("cancel");
    let _ = collect_until_terminal(&mut events).await.expect("collect");

    // Reopen proves the entries are durable, not runtime-only.
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::with_cwd(std::env::temp_dir()),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let snapshot = session.snapshot().await.expect("snapshot after reopen");
    let shell_count = snapshot
        .entries
        .iter()
        .filter(|entry| matches!(entry, SessionEntry::ShellExecution { .. }))
        .count();
    assert_eq!(shell_count, 2, "shell entries survive reopen");
    session.close().await.expect("close reopened");
    runtime.join().await.expect("join reopened");
}

#[tokio::test]
async fn shell_passthrough_cancel_settles_durably_with_cancelled_flag() {
    let db = temp_db("shell-cancel");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::with_cwd(std::env::temp_dir()),
        store.clone(),
    );
    let session = runtime.session();

    // Drive run_shell and esc-cancel: the reply returns at intent
    // commit, so cancel and settlement are observed as events while the
    // process is still in flight.
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session
        .run_shell("sleep 30", false)
        .await
        .expect("shell passthrough accepted");
    let cancelled = session.cancel_shell().await.expect("cancel command");
    assert!(cancelled, "a passthrough was in flight");
    let mut settled_cancelled = false;
    while !settled_cancelled {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), events.recv())
            .await
            .expect("event within 10s")
            .expect("event stream alive");
        settled_cancelled = matches!(
            event,
            RuntimeEvent::ShellSettled {
                cancelled: true,
                ..
            }
        );
    }

    let snapshot = session.snapshot().await.expect("snapshot");
    match snapshot.entries.last() {
        Some(SessionEntry::ShellExecution {
            command,
            cancelled: true,
            exit_code: None,
            ..
        }) if command == "sleep 30" => {}
        other => panic!("cancelled shell entry not durable: {other:?}"),
    }
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn shell_marker_owns_the_branch_until_settled() {
    let db = temp_db("shell-branch-guard");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::with_cwd(std::env::temp_dir()),
        store.clone(),
    );
    let session = runtime.session();

    // Accept a slow passthrough; the marker owns the branch, so both
    // submit and next-run must refuse with a clear error.
    session
        .run_shell("sleep 5", false)
        .await
        .expect("shell accepted");
    let err = session
        .submit_if_idle("meanwhile")
        .await
        .expect_err("submit refused while shell runs");
    assert!(
        matches!(err, CommandError::ShellPassthroughBusy),
        "expected shell busy, got {err:?}"
    );
    let err = session
        .next_run("queued meanwile")
        .await
        .expect_err("next-run refused while shell runs");
    assert!(
        matches!(err, CommandError::ShellPassthroughBusy),
        "expected shell busy, got {err:?}"
    );

    // A second passthrough is also refused.
    let err = session
        .run_shell("true", false)
        .await
        .expect_err("one passthrough at a time");
    assert!(
        matches!(err, CommandError::ShellPassthroughBusy),
        "expected shell busy, got {err:?}"
    );

    // After settlement the lane accepts work again.
    wait_for_entry(&session, "sleep 5").await;
    let op = session
        .submit_if_idle("after")
        .await
        .expect("submit after settlement");
    session.cancel(op).await.expect("cancel");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
