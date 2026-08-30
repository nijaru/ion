//! Crash recovery tests.

use super::support::*;

// ---- Crash-window recovery (DESIGN.md §32 Step 3, §30.2) ----

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_after_durable_model_intent() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ModelExecution);
    let provider = SharedLogProvider::default();
    let runtime = Runtime::start_with_effect_gate(
        provider.clone(),
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, _events) = session.subscribe().await.expect("subscribe");
    let submit_session = session.clone();
    let submit = tokio::spawn(async move { submit_session.submit_if_idle("goal").await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("effect gate reached");

    assert!(
        provider.requests().is_empty(),
        "the provider must not start before the gate"
    );
    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(checkpoint.state, OperationState::AssistantEffectPending);
    assert!(checkpoint.open_effect.is_some());

    // Abort exactly at the committed-intent / external-execution boundary.
    runtime.crash();
    gate.release();
    let _ = submit.await.expect("submit task");
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
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::AssistantTextDelta { text, .. } if text == "recovered\n"
    )));
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_after_model_settlement() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ModelSettlement);
    let runtime = Runtime::start_with_effect_gate(
        ScriptedProvider::new(vec![ScriptedMessage::text("before crash\n")]),
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, _events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("effect gate reached");

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(checkpoint.state, OperationState::AssistantEffectPending);
    assert!(checkpoint.open_effect.is_some());

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
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_before_tool_execution() {
    let dir = std::env::temp_dir().join(format!("ion-gate-read-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("note.txt"), "persisted bytes").expect("write");
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ToolExecution);
    let runtime = Runtime::start_with_effect_gate(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "read",
            json!({"path": "note.txt"}),
        )]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, _events) = session.subscribe().await.expect("subscribe");
    let submit_session = session.clone();
    let submit = tokio::spawn(async move { submit_session.submit_if_idle("read").await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("effect gate reached");

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(matches!(
        checkpoint.state,
        OperationState::ToolEffectPending { .. }
    ));
    assert!(checkpoint.open_effect.is_some());
    assert!(
        !loaded
            .entries
            .iter()
            .any(|record| matches!(&record.entry, SessionEntry::ToolResult { .. }))
    );

    runtime.crash();
    gate.release();
    let _ = submit.await.expect("submit task");
    drop(session);
    drop(runtime);

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("recovered\n")]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn replay_safe_recovery_does_not_reacquire_a_structurally_removed_tool() {
    let dir = std::env::temp_dir().join(format!("ion-gate-removed-read-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("note.txt"), "must not be reread").expect("write");
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ToolExecution);
    let runtime = Runtime::start_with_effect_gate(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "read",
            json!({"path": "note.txt"}),
        )]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, _events) = session.subscribe().await.expect("subscribe");
    let submit_session = session.clone();
    let submit = tokio::spawn(async move { submit_session.submit_if_idle("read").await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("tool gate reached");

    runtime.crash();
    gate.release();
    let _ = submit.await.expect("submit task");
    drop(session);
    drop(runtime);

    let loaded = store.load(session_id).await.expect("load");
    let mut config = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("main lane")
        .config
        .clone();
    config.tools =
        crate::tool::ToolSelection::Only(std::collections::BTreeSet::from(["find".to_owned()]));
    store
        .set_lane_config(session_id, crate::session::lane::MAIN, config)
        .await
        .expect("remove read capability");

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("recovered\n")]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));

    let loaded = store.load(session_id).await.expect("reload");
    assert!(loaded.entries.iter().any(|record| matches!(
        &record.entry,
        SessionEntry::ToolResult {
            result: ToolResult::Err { error, .. }
        } if error.contains("unknown tool: read")
    )));
    assert!(!loaded.entries.iter().any(|record| matches!(
        &record.entry,
        SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. }
        } if output.contains("must not be reread")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_after_tool_execution() {
    let dir = std::env::temp_dir().join(format!("ion-gate-settle-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("note.txt"), "persisted bytes").expect("write");
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ToolSettlement);
    let runtime = Runtime::start_with_effect_gate(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "read",
            json!({"path": "note.txt"}),
        )]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, _events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("read").await.expect("submit");
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("effect gate reached");

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(matches!(
        checkpoint.state,
        OperationState::ToolEffectPending { .. }
    ));
    assert!(checkpoint.open_effect.is_some());

    runtime.crash();
    gate.release();
    drop(session);
    drop(runtime);

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("recovered\n")]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let loaded = store.load(session_id).await.expect("load");
    assert!(loaded.entries.iter().any(|record| matches!(
        &record.entry,
        SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. }
        } if output.contains("persisted bytes")
    )));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn effect_gate_close_waits_for_suspend_commit() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::CloseSuspendCommit);
    let runtime = Runtime::start_with_effect_gate(
        SharedLogProvider {
            settle_delay: Duration::from_millis(250),
            ..SharedLogProvider::default()
        },
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
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

    let close_session = session.clone();
    let close = tokio::spawn(async move { close_session.close().await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("suspend gate reached");
    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(matches!(
        checkpoint.state,
        OperationState::AssistantEffectPending
    ));

    gate.release();
    close.await.expect("close task").expect("close");
    runtime.join().await.expect("join");
    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(matches!(checkpoint.state, OperationState::Suspended));
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_before_compaction_execution() {
    let probe = CompactionProbe::with_window(128_000);
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::CompactionExecution);
    let runtime = Runtime::start_with_effect_gate(
        probe.clone(),
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }
    session.steer("and also this").await.expect("steer");
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("compaction gate reached");

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(matches!(
        checkpoint.state,
        OperationState::CompactionPending
    ));
    assert!(checkpoint.open_effect.is_some());
    assert!(
        loaded
            .entries
            .iter()
            .all(|record| !matches!(&record.entry, SessionEntry::Compaction { .. }))
    );

    runtime.crash();
    gate.release();
    drop(session);
    drop(runtime);

    let runtime = Runtime::open_session(probe, ToolRegistry::default(), store.clone(), session_id)
        .await
        .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let loaded = store.load(session_id).await.expect("load");
    assert!(
        loaded
            .entries
            .iter()
            .any(|record| matches!(&record.entry, SessionEntry::Compaction { .. }))
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_after_durable_cancellation() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::CancellationSignal);
    let runtime = Runtime::start_with_effect_gate(
        SharedLogProvider {
            settle_delay: Duration::from_secs(30),
            ..SharedLogProvider::default()
        },
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("cancel me").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }

    let cancel_session = session.clone();
    let cancel = tokio::spawn(async move { cancel_session.cancel(operation_id).await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("cancellation gate reached");
    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(checkpoint.cancel_requested);
    assert!(matches!(
        checkpoint.state,
        OperationState::AssistantEffectPending
    ));

    runtime.crash();
    gate.release();
    let _ = cancel.await.expect("cancel task");
    drop(session);
    drop(runtime);

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("late provider result")]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::OperationCancelled { operation_id: id, .. } if *id == operation_id
    )));
    let loaded = store.load(session_id).await.expect("load");
    assert!(matches!(
        loaded.operations[0].latest.1.state,
        OperationState::Finished(OperationOutcome::Cancelled)
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_with_a_durable_queued_operation() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::PendingNextRunCommit);
    let runtime = Runtime::start_with_effect_gate(
        SharedLogProvider {
            settle_delay: Duration::from_secs(30),
            ..SharedLogProvider::default()
        },
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("first").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }

    let enqueue_session = session.clone();
    let enqueue = tokio::spawn(async move { enqueue_session.next_run("second").await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("queue gate reached");
    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(loaded.operations.len(), 1);
    let pending = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("main lane")
        .state
        .pending_next_run
        .as_ref()
        .expect("durable next run");
    assert_eq!(pending.prompt, "second");
    assert!(
        loaded
            .entries
            .iter()
            .all(|entry| entry.id != pending.entry_id),
        "pending input must not exist as a semantic entry before acceptance"
    );

    runtime.crash();
    gate.release();
    let _ = enqueue.await.expect("enqueue task");
    drop(session);
    drop(runtime);

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("first recovered"),
            ScriptedMessage::text("second recovered"),
        ]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let first = collect_until_terminal(&mut events).await.expect("first");
    assert!(matches!(
        first.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let second = collect_until_terminal(&mut events).await.expect("second");
    assert!(matches!(
        second.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(
        loaded
            .entries
            .iter()
            .filter(|record| matches!(&record.entry, SessionEntry::UserMessage { .. }))
            .count(),
        2
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn interrupted_recovery_can_restart_from_its_replacement_intent() {
    let store = SessionStore::open_in_memory().expect("store");
    let initial_gate = EffectGate::new(EffectBoundary::ModelExecution);
    let initial = Runtime::start_with_effect_gate(
        SharedLogProvider::default(),
        ToolRegistry::default(),
        store.clone(),
        initial_gate.clone(),
    );
    let session_id = initial.session_id();
    let session = initial.session();
    let submit_session = session.clone();
    let submit = tokio::spawn(async move { submit_session.submit_if_idle("goal").await });
    timeout(Duration::from_secs(2), initial_gate.wait_until_reached())
        .await
        .expect("initial gate reached");
    initial.crash();
    initial_gate.release();
    let _ = submit.await.expect("submit task");
    drop(session);
    drop(initial);

    let recovering = SharedLogProvider {
        settle_delay: Duration::from_secs(30),
        ..SharedLogProvider::default()
    };
    let runtime = Runtime::open_session(
        recovering.clone(),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("first reopen");
    for _ in 0..100 {
        if !recovering.requests().is_empty() {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(recovering.requests().len(), 1, "recovery provider started");
    runtime.crash();
    drop(runtime);

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_millis(50),
            "recovered twice",
        )]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("second reopen");
    let session = runtime.session();
    for _ in 0..100 {
        let loaded = store.load(session_id).await.expect("poll recovery");
        if matches!(
            loaded.operations[0].latest.1.state,
            OperationState::Finished(OperationOutcome::Completed)
        ) {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    let loaded = store.load(session_id).await.expect("load recovered");
    assert!(
        matches!(
            loaded.operations[0].latest.1.state,
            OperationState::Finished(OperationOutcome::Completed)
        ),
        "final recovery state: {:?}",
        loaded.operations[0].latest.1.state
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn crash_during_model_step_recovers_by_replay() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        SharedLogProvider {
            settle_delay: Duration::from_secs(30),
            ..SharedLogProvider::default()
        },
        ToolRegistry::default(),
        store.clone(),
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
    assert_eq!(
        store
            .load(session_id)
            .await
            .expect("load")
            .assistant_frames
            .len(),
        1
    );

    // Process loss mid-model-step: no close, no settlement.
    runtime.crash();
    drop(runtime);
    drop(session);

    // Reopen: the pending model step replays with a bumped attempt.
    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("recovered\n")]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded.iter().any(|e| matches!(
            e,
            RuntimeEvent::AssistantTextDelta { text, .. } if text == "recovered\n"
        )),
        "the replayed model step must stream: {recorded:?}"
    );
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(
        checkpoint.state,
        OperationState::Finished(OperationOutcome::Completed)
    );
    assert!(loaded.assistant_frames.is_empty());
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn crash_during_replayable_tool_recovers_by_reexecution() {
    let dir = std::env::temp_dir().join(format!("ion-crash-read-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("note.txt"), "persisted bytes").expect("write");

    let store = SessionStore::open_in_memory().expect("store");
    let registry = ToolRegistry::with_cwd(&dir);
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "read",
            json!({"path":"note.txt"}),
        )]),
        registry,
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("read it").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::ToolStarted { .. }) {
            break;
        }
    }

    // Process loss while the read effect is in flight.
    runtime.crash();
    drop(runtime);
    drop(session);

    // Reopen: read is ReplaySafe, so it re-executes and the operation
    // continues into the next model step.
    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("after recovery\n")]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(
        checkpoint.state,
        OperationState::Finished(OperationOutcome::Completed)
    );
    assert!(loaded.entries.iter().any(|record| matches!(
        &record.entry,
        SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. },
        } if output.contains("persisted bytes")
    )));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn crash_during_bash_settles_indeterminate_and_stays_usable() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "bash",
            json!({"command":"sleep 30"}),
        )]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let indeterminate_operation = session.submit_if_idle("run").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::ToolStarted { .. }) {
            break;
        }
    }

    // Process loss while a NeverReplay effect is in flight.
    runtime.crash();
    drop(runtime);
    drop(session);

    // Reopen: bash must NOT re-execute; the operation settles as
    // indeterminate and the session stays usable.
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
    assert_eq!(snapshot.operation, OperationStatus::Idle);
    assert_eq!(
        snapshot.latest_settlement,
        Some(crate::OperationSettlement {
            operation_id: indeterminate_operation,
            outcome: OperationOutcome::Indeterminate,
        })
    );
    let warning = snapshot
        .indeterminate
        .as_ref()
        .expect("recovery warning must survive until a frontend attaches");
    assert!(warning.message.contains("inspect it before retrying"));

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(
        checkpoint.state,
        OperationState::Finished(OperationOutcome::Indeterminate)
    );
    assert!(
        !loaded
            .entries
            .iter()
            .any(|record| matches!(&record.entry, SessionEntry::ToolResult { .. }))
    );

    // The session accepts new work after the indeterminate settlement.
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("next").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == operation_id
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn worker_indeterminate_does_not_leak_into_main_snapshot() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "bash",
            json!({"command":"sleep 30"}),
        )]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    session.create_lane("worker").await.expect("worker lane");
    let (_snapshot, mut all_events) = session.subscribe_all().await.expect("subscribe all");
    let worker_operation = session
        .submit_if_idle_on_lane("worker", "run")
        .await
        .expect("worker submit");
    loop {
        let event = timeout(Duration::from_secs(2), all_events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(
            event,
            RuntimeEvent::ToolStarted { operation_id, .. } if operation_id == worker_operation
        ) {
            break;
        }
    }

    runtime.crash();
    drop(runtime);
    drop(session);

    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let snapshot = session.snapshot().await.expect("main snapshot");
    assert!(snapshot.indeterminate.is_none());
    assert!(snapshot.latest_settlement.is_none());
    assert_eq!(snapshot.operation, OperationStatus::Idle);

    let loaded = store.load(session_id).await.expect("load");
    let worker = loaded
        .operations
        .iter()
        .find(|operation| operation.id == worker_operation)
        .expect("worker operation");
    assert_eq!(
        worker.latest.1.state,
        OperationState::Finished(OperationOutcome::Indeterminate)
    );

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn parked_approval_survives_process_loss_and_decides_after_reopen() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_interactive_with_effect_gate(
        ScriptedProvider::new(vec![
            ScriptedMessage::tool("bash", json!({ "command": "echo hi" })),
            ScriptedMessage::text("done"),
        ]),
        ToolRegistry::default(),
        store.clone(),
        EffectGate::new(EffectBoundary::ToolExecution),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    wait_for_park(&mut events).await;

    // The park is durable: the staged call is in the checkpoint and
    // nothing has executed.
    let loaded = store.load(session_id).await.expect("load");
    assert!(matches!(
        loaded.operations[0].latest.1.state,
        OperationState::ApprovalPending { .. }
    ));
    assert!(
        !loaded
            .entries
            .iter()
            .any(|record| matches!(&record.entry, SessionEntry::ToolResult { .. })),
        "nothing may have executed before the decision"
    );

    // Process loss: drop the live runtime; reopen interactively.
    drop(session);
    runtime.crash();
    drop(runtime);

    let runtime = Runtime::open_interactive(
        ScriptedProvider::new(vec![ScriptedMessage::text("done")]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
        Arc::new(crate::policy::DefaultPolicy),
        Vec::new(),
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    // The snapshot is authoritative: the parked decision is still live,
    // so the frontend can re-surface it and the user can decide.
    wait_for_state(&session, |state| {
        matches!(state, OperationState::ApprovalPending { .. })
    })
    .await;
    let snapshot = session.snapshot().await.expect("snapshot");
    let operation_id = match snapshot.operation {
        OperationStatus::Active { operation_id, .. } => operation_id,
        other => panic!("the parked operation must still be active: {other:?}"),
    };
    session
        .decide_approval(operation_id, true)
        .await
        .expect("approve");
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded.iter().any(|e| matches!(
            e,
            RuntimeEvent::ToolSettled {
                is_error: false,
                ..
            }
        )),
        "the approved call executes after the crash: {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. }))
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Durable: exactly one tool result for the approved call.
    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(
        loaded
            .entries
            .iter()
            .filter(|record| matches!(&record.entry, SessionEntry::ToolResult { .. }))
            .count(),
        1
    );
}
