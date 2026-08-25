//! Regressions tests.

use super::support::*;

// ---- Dogfood round 2 regressions ----

#[tokio::test]
async fn find_matches_nested_paths_against_the_search_root() {
    // Found live: nested files were tested as bare names because the
    // walk stripped each visited directory instead of the search root.
    let dir = tempfile::tempdir().expect("tmpdir");
    std::fs::create_dir_all(dir.path().join("crates").join("alpha")).expect("mkdir");
    std::fs::create_dir_all(dir.path().join("crates").join("beta")).expect("mkdir");
    std::fs::write(dir.path().join("crates").join("alpha").join("C.toml"), "x").expect("write");
    std::fs::write(dir.path().join("crates").join("beta").join("C.toml"), "x").expect("write");
    std::fs::write(dir.path().join("top.toml"), "x").expect("write");

    let registry = crate::ToolRegistry::read_only(dir.path());
    let outcome = registry
        .execute(
            "find",
            &json!({ "pattern": "crates/*/C.toml" }),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.output);
    let mut found: Vec<&str> = outcome.output.lines().collect();
    found.sort_unstable();
    assert_eq!(
        found,
        vec!["crates/alpha/C.toml", "crates/beta/C.toml"],
        "{outcome:?}"
    );
}

#[tokio::test]
async fn path_resolution_containment_survives_a_relative_cwd() {
    // Found live: with registry cwd ".", normalization dropped the
    // CurDir component and every subpath read as an escape.
    let dir = tempfile::tempdir().expect("tmpdir");
    std::fs::create_dir_all(dir.path().join("crates")).expect("mkdir");
    std::fs::write(dir.path().join("crates").join("a.txt"), "x").expect("write");

    // "." as cwd: the delegate child configuration.
    let registry = crate::ToolRegistry::read_only(".");
    // The tempdir must be the process cwd for this probe.
    let prev = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(dir.path()).expect("chdir");
    let outcome = registry
        .execute(
            "find",
            &json!({ "path": "crates", "pattern": "*.txt" }),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    std::env::set_current_dir(prev).expect("restore cwd");
    assert!(!outcome.is_error, "{}", outcome.output);
    assert!(outcome.output.contains("a.txt"), "{outcome:?}");
}

// ---- Display-only surfaces: thinking deltas + tool previews ----

#[tokio::test]
async fn thinking_deltas_stream_but_stay_out_of_the_transcript() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![
            ScriptedMessage::Thinking {
                text: "pondering the request".to_owned(),
            },
            ScriptedMessage::text("answer\n"),
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("hi").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");

    let mut thinking = Vec::new();
    for event in &recorded {
        if let RuntimeEvent::ThinkingDelta { text, .. } = event {
            thinking.push(text.clone());
        }
    }
    assert_eq!(thinking, vec!["pondering the request".to_owned()]);
    // Text is unaffected and the operation finished cleanly.
    assert_eq!(texts(&recorded), vec!["answer\n".to_owned()]);
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    // Thinking never becomes a durable entry (display-only surface).
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().all(|entry| match entry {
        SessionEntry::AssistantMessage { text } => text == "answer\n",
        _ => true,
    }));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn tool_settled_event_carries_a_bounded_preview() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool(
            "bash",
            json!({"command":"for i in $(seq 1 60); do echo line-$i; done"}),
        ),
        ScriptedMessage::text("done\n"),
    ]);
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");

    let previews: Vec<Option<String>> = recorded
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::ToolSettled { preview, .. } => Some(preview.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(previews.len(), 1);
    let preview = previews[0].as_ref().expect("bash produced output");
    // Tail-truncated: keeps the end, bounds the head.
    assert!(preview.contains("line-60"));
    assert!(!preview.contains("line-1\n"));
    assert!(preview.starts_with('…'));
    assert!(preview.lines().count() <= 21);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn switching_provider_swaps_models_at_step_boundaries() {
    use crate::provider::SwitchingProvider;

    // Two scripted providers with distinguishable replies; a switch
    // mid-operation lands on the next step's reply.
    let provider = SwitchingProvider::new(
        "a",
        ScriptedProvider::new(vec![
            ScriptedMessage::text("from-a\n"),
            ScriptedMessage::text("still-a\n"),
        ]),
    );
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert_eq!(
        texts(&recorded),
        vec!["from-a\n".to_owned(), "still-a\n".to_owned()]
    );

    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // A durable mid-session change lands on the next operation's steps.
    // The host factory resolves each exact model id; selection itself
    // lives in the session.
    let make: std::sync::Arc<dyn Fn(String) -> ScriptedProvider + Send + Sync> =
        std::sync::Arc::new(|m| {
            ScriptedProvider::new(vec![ScriptedMessage::text(format!("switched-to-{m}\n"))])
        });
    let provider = SwitchingProvider::switchable(
        "a",
        ScriptedProvider::new(vec![ScriptedMessage::text("first\n")]),
        make,
    );
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert_eq!(texts(&recorded), vec!["first\n".to_owned()]);

    let previous = session.switch_model("b").await.expect("switch");
    assert_eq!(previous, "a");
    session.submit_if_idle("again").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert_eq!(texts(&recorded), vec!["switched-to-b\n".to_owned()]);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn model_switch_refuses_an_id_the_resolver_cannot_build() {
    // A fixed provider accepts only its own model; the refusal happens
    // before any durable change.
    use crate::provider::SwitchingProvider;

    let provider = SwitchingProvider::new("a", ScriptedProvider::echo());
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let err = session.switch_model("nonexistent").await;
    assert!(matches!(err, Err(crate::CommandError::UnsupportedModel(_))));
    // The accepted model is unchanged and still works.
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert_eq!(texts(&recorded), vec!["ok".to_owned()]);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn reasoning_draft_clears_on_provider_failure() {
    // Found in review: only the Completed path cleared the live
    // reasoning buffer; failure paths retained stale reasoning into
    // the next operation.
    let runtime = start_runtime(
        ScriptedProvider::new(vec![
            ScriptedMessage::Thinking {
                text: "pondering".to_owned(),
            },
            ScriptedMessage::Fail {
                message: "provider exploded".to_owned(),
            },
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFailed { .. })
    ));

    // The next operation must not resurrect the failed step's reasoning
    // (the exhausted script completes silently with no deltas).
    session.submit_if_idle("again").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .all(|event| !matches!(event, RuntimeEvent::ThinkingDelta { .. }))
    );

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn crash_recovery_replays_with_the_persisted_model_not_the_launch_default() {
    // DESIGN.md §14.8: the restart's launch default must never
    // substitute for a pending step's frozen model snapshot.
    use crate::provider::SwitchingProvider;
    use std::sync::Arc;

    let db = temp_db("model-recovery");
    let store = SessionStore::open(&db).expect("store");
    let make: Arc<dyn Fn(String) -> ScriptedProvider + Send + Sync> = Arc::new(|m: String| {
        ScriptedProvider::new(vec![ScriptedMessage::text(format!("served-by-{m}\n"))])
    });
    let provider = SwitchingProvider::switchable(
        "a",
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "never arrives",
        )]),
        Arc::clone(&make),
    );
    let runtime = Runtime::start_with_policy(
        provider,
        ToolRegistry::default(),
        store.clone(),
        permissive_policy(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    session.submit_if_idle("goal").await.expect("submit");
    wait_for_state(&session, |state| {
        matches!(state, OperationState::AssistantEffectPending)
    })
    .await;

    // Process loss mid-model-step under model "a".
    runtime.crash();
    drop(runtime);
    drop(session);

    // Reopen composed for model "b": recovery must still serve the
    // pending step from "a" via the resolver, never from "b".
    let make_reopen: Arc<dyn Fn(String) -> ScriptedProvider + Send + Sync> = Arc::new(|m| {
        if m == "a" {
            ScriptedProvider::new(vec![ScriptedMessage::text("recovered-under-a\n")])
        } else {
            ScriptedProvider::new(vec![ScriptedMessage::text(format!("served-by-{m}\n"))])
        }
    });
    let reopen_provider = SwitchingProvider::switchable(
        "b",
        ScriptedProvider::new(vec![ScriptedMessage::text("served-by-b\n")]),
        make_reopen,
    );
    let runtime = Runtime::open_session(
        reopen_provider,
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    // The persisted selection survives restart: the "b" launch default
    // never becomes this session's authority.
    let snapshot = session.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.model_ref, "a");

    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded.iter().any(|e| matches!(
            e,
            RuntimeEvent::AssistantTextDelta { text, .. } if text == "recovered-under-a\n"
        )),
        "recovery must replay the persisted model: {recorded:?}"
    );
    assert!(
        !recorded.iter().any(|e| matches!(
            e,
            RuntimeEvent::AssistantTextDelta { text, .. } if text.contains("served-by")
        )),
        "the launch default must not serve the recovered step"
    );
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));

    // §11.3: each attempt row names the exact model that ran.
    let connection = rusqlite::Connection::open(&db).expect("open db");
    let refs: Vec<(String, String, String, String, String, String)> = connection
        .prepare(
            "SELECT model_ref, capability_snapshot_id, context_manifest_id,
                    capabilities, context_fingerprint, cache_expectation
             FROM model_steps ORDER BY created_at",
        )
        .expect("prepare")
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    assert_eq!(refs.len(), 2, "{refs:?}");
    assert!(refs.iter().all(
        |(model, snapshot, manifest, capabilities, fingerprint, expectation)| {
            let capabilities: crate::provider::ModelCapabilities =
                serde_json::from_str(capabilities).expect("capabilities json");
            model == "a"
                && snapshot.len() == 64
                && manifest.len() == 64
                && fingerprint.len() == 64
                && expectation == "unsupported"
                && capabilities.tool_calls
                && !capabilities.prompt_cache
        }
    ));
    let snapshot_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM capability_snapshots", [], |row| {
            row.get(0)
        })
        .expect("snapshot count");
    let manifest_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM context_manifests", [], |row| {
            row.get(0)
        })
        .expect("manifest count");
    assert_eq!(snapshot_count, 1);
    assert_eq!(manifest_count, 1);
    let effect_inputs: Vec<String> = connection
        .prepare("SELECT effective_input FROM effects WHERE kind = 'model_step'")
        .expect("prepare effects")
        .query_map([], |row| row.get(0))
        .expect("query effects")
        .collect::<Result<Vec<_>, _>>()
        .expect("effect rows");
    assert!(effect_inputs.iter().all(|input| {
        let value: serde_json::Value = serde_json::from_str(input).expect("effect json");
        value.get("tools").is_none()
            && value.get("capability_snapshot").is_none()
            && value.get("context_manifest").is_none()
    }));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn event_lag_is_signaled_reliably_and_snapshot_carries_live_draft() {
    // DESIGN.md §21.4: a full queue cannot be used to enqueue its own
    // overflow signal, so lag is detected by the receiver against the
    // ring tail; the fresh snapshot then carries the authoritative
    // draft for reconstruction.
    let mut messages: Vec<ScriptedMessage> = (0..80)
        .map(|i| ScriptedMessage::text(format!("d{i} ")))
        .collect();
    // Keep the operation in flight so the live draft survives until
    // the resubscribe.
    messages.push(ScriptedMessage::delayed(Duration::from_secs(30), "never"));
    let runtime = start_runtime(ScriptedProvider::new(messages), ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");

    // Do not read while 80 deltas overflow the 64-slot ring.
    sleep(Duration::from_millis(300)).await;
    let first = timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("recv");
    assert!(
        matches!(&first, Err(RuntimeError::SubscriptionLagged)),
        "lag must be delivered reliably, got {first:?}"
    );

    let (fresh_snapshot, mut fresh) = session.subscribe().await.expect("resubscribe");
    let live = fresh_snapshot.live.as_ref().expect("live state present");
    assert!(live.draft_text.contains("d0 "));
    assert!(live.draft_text.contains("d79 "));
    assert!(live.pending_tools.is_empty());

    // The fresh stream continues from the tail without lag errors;
    // only the still-pending delayed delta remains when we stop.
    while let Ok(Ok(_)) = timeout(Duration::from_millis(100), fresh.recv()).await {}

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn cancel_during_lag_is_visible_after_resubscribe() {
    // §21.4: critical lifecycle events are never silently dropped. A
    // subscriber that lags and resubscribes must see the terminal
    // state in the snapshot, not a stream that just goes quiet.
    let mut messages: Vec<ScriptedMessage> = (0..80)
        .map(|i| ScriptedMessage::text(format!("d{i} ")))
        .collect();
    messages.push(ScriptedMessage::delayed(Duration::from_secs(30), "never"));
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_policy(
        ScriptedProvider::new(messages),
        ToolRegistry::default(),
        store.clone(),
        permissive_policy(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("goal").await.expect("submit");
    // Overflow the ring without reading, then cancel mid-step.
    sleep(Duration::from_millis(300)).await;
    session.cancel(operation_id).await.expect("cancel");
    sleep(Duration::from_millis(200)).await;

    let first = timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("recv");
    assert!(matches!(first, Err(RuntimeError::SubscriptionLagged)));

    let (fresh, _events) = session.subscribe().await.expect("resubscribe");
    assert_eq!(fresh.operation, OperationStatus::Idle);
    assert!(fresh.live.is_none());
    for attempt in 0..5 {
        match store.load(runtime.session_id()).await {
            Ok(loaded) => {
                let (_, checkpoint) = &loaded.operations[0].latest;
                assert_eq!(
                    checkpoint.state,
                    OperationState::Finished(OperationOutcome::Cancelled)
                );
                break;
            }
            Err(err) => {
                eprintln!("attempt {attempt}: {err:?}");
                assert!(attempt < 4, "load never succeeded");
                sleep(Duration::from_millis(100)).await;
            }
        }
    }

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
