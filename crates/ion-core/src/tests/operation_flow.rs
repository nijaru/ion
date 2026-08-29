//! Operation flow tests.

use super::support::*;

// ---- Operation-level integration tests ----

#[tokio::test]
async fn tool_loop_success_admits_tools_and_finishes() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool("bash", json!({"command":"echo tool-said-hello"})),
        ScriptedMessage::text("final answer\n"),
    ]);
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");

    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == operation_id
    ));
    // Tool execution is a live event; tool output is a semantic entry,
    // never an assistant text delta (DESIGN.md §16.4).
    assert!(recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolStarted { tool, .. } if tool == "bash"
    )));
    assert_eq!(texts(&recorded), vec!["final answer\n".to_owned()]);
    assert!(recorded.iter().all(|e| !matches!(
        e,
        RuntimeEvent::OperationFailed { .. } | RuntimeEvent::OperationCancelled { .. }
    )));
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. },
        } if output.contains("tool-said-hello")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn file_backed_runtime_persists_bounded_result_and_raw_artifact() {
    let data = tempfile::tempdir().expect("data directory");
    let store = SessionStore::open(data.path().join("sessions.db")).expect("file-backed store");
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool(
            "bash",
            json!({"command":"i=0; while [ \"$i\" -lt 20000 ]; do printf x; i=$((i+1)); done"}),
        ),
        ScriptedMessage::text("done\n"),
    ]);
    let runtime = Runtime::start_with_policy(
        provider,
        ToolRegistry::default(),
        store,
        permissive_policy(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|event| matches!(event, RuntimeEvent::OperationFinished { .. }))
    );

    let snapshot = session.snapshot().await.expect("snapshot");
    let (output, artifact) = snapshot
        .entries
        .iter()
        .find_map(|entry| match entry {
            SessionEntry::ToolResult {
                result: ToolResult::Ok {
                    output, artifact, ..
                },
            } => Some((output.clone(), artifact.clone())),
            _ => None,
        })
        .expect("durable bash result");
    let artifact = artifact.expect("durable raw artifact");
    assert!(output.contains("tool output abbreviated"));
    assert!(output.len() <= 16 * 1024);
    assert_eq!(artifact.total_bytes, 20_000);
    let id = artifact
        .uri
        .strip_prefix("artifact://")
        .expect("artifact URI");
    let raw = std::fs::read(data.path().join("artifacts").join(id)).expect("raw artifact");
    assert_eq!(raw, vec![b'x'; 20_000]);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn tool_error_is_model_visible_and_operation_continues() {
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool(
        "read",
        json!({"path":"definitely-not-here.txt"}),
    )]);
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    // An expected tool failure is a model-visible outcome, not a harness
    // failure (DESIGN.md §16.5): the operation completes.
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == operation_id
    ));
    assert!(!recorded.iter().any(|e| matches!(
        e,
        RuntimeEvent::OperationFailed { .. } | RuntimeEvent::OperationCancelled { .. }
    )));
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Err { error, .. },
        } if error.contains("read failed") || error.contains("No such file")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn malformed_args_are_denied_before_the_effect_starts() {
    let provider =
        ScriptedProvider::new(vec![ScriptedMessage::tool("read", json!({"bogus": true}))]);
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    // No ToolStarted: the effect never started (DESIGN.md §17.3).
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ToolStarted { .. }))
    );
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Err { error, .. },
        } if error.contains("path")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn unknown_tool_is_denied_before_the_effect_starts() {
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool("frobnicate", json!({}))]);
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ToolStarted { .. }))
    );
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Err { error, .. },
        } if error.contains("unknown tool")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn cancel_during_tool_cancels_operation_and_kills_process() {
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool(
        "bash",
        json!({"command":"sleep 30 && echo PWNED"}),
    )]);
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("go").await.expect("submit");

    // Wait until the tool effect has actually started.
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::ToolStarted { .. }) {
            break;
        }
    }
    // Give the bash child a moment to spawn before cancelling.
    sleep(STEP * 4).await;

    session.cancel(operation_id).await.expect("cancel");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(recorded.iter().any(
        |e| matches!(e, RuntimeEvent::OperationCancelled { operation_id: id, .. } if *id == operation_id)
    ));
    assert!(!recorded.iter().any(|e| matches!(
        e,
        RuntimeEvent::AssistantTextDelta { text, .. } if text.contains("PWNED")
    )));

    let close = session.close().await;
    let join = timeout(Duration::from_secs(5), runtime.join());
    assert!(close.is_ok(), "close: {close:?}");
    assert!(join.await.is_ok(), "runtime should join after cancel");
}

#[tokio::test]
async fn tool_loop_multiple_calls_run_sequentially_in_one_operation() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool("bash", json!({"command":"echo one"})),
        ScriptedMessage::tool("bash", json!({"command":"echo two"})),
        ScriptedMessage::text("done\n"),
    ]);
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == operation_id
    ));
    let started_tools: Vec<&str> = recorded
        .iter()
        .filter_map(|e| match e {
            RuntimeEvent::ToolStarted { tool, .. } => Some(tool.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(started_tools, ["bash", "bash"]);
    assert_eq!(texts(&recorded), vec!["done\n".to_owned()]);
    let snapshot = session.snapshot().await.expect("snapshot");
    let outputs: Vec<String> = snapshot
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ToolResult {
                result: ToolResult::Ok { output, .. },
            } => Some(output.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(outputs, vec!["one\n".to_owned(), "two\n".to_owned()]);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn model_step_request_carries_step_tool_specs() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool("read", json!({"path":"Cargo.toml"})),
        ScriptedMessage::text("done"),
    ]);
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. },
        } if output.contains("ion-core")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn admitted_scope_refreshes_without_admitting_an_unrelated_scope() {
    struct DynamicTool(&'static str);
    impl Tool for DynamicTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.0.to_owned(),
                description: "dynamic test capability".to_owned(),
                input_schema: json!({"type": "object", "required": []}),
            }
        }

        fn call<'a>(
            &'a self,
            _arguments: serde_json::Value,
            _cancel: CancellationToken,
        ) -> std::pin::Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
            Box::pin(async { ToolOutcome::text("dynamic") })
        }
    }

    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(100),
        ..SharedLogProvider::default()
    };
    let catalog = ToolCatalog::default();
    catalog.register_scope("dynamic", vec![Arc::new(DynamicTool("dynamic-v1"))]);
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_policy(
        provider.clone(),
        catalog.clone(),
        store,
        permissive_policy(),
    );
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
    catalog.register_scope("dynamic", vec![Arc::new(DynamicTool("dynamic-v2"))]);
    catalog.register_scope("unrelated", vec![Arc::new(DynamicTool("unrelated"))]);
    session.steer("continue").await.expect("steer");
    collect_until_terminal(&mut events).await.expect("collect");
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "dynamic-v1")
    );
    assert!(
        !requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "dynamic-v2")
    );
    assert!(
        requests[1]
            .tools
            .iter()
            .any(|tool| tool.name == "dynamic-v2")
    );
    assert!(
        !requests[1]
            .tools
            .iter()
            .any(|tool| tool.name == "dynamic-v1")
    );
    assert!(
        !requests[1]
            .tools
            .iter()
            .any(|tool| tool.name == "unrelated")
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn steer_projection_reaches_the_next_model_step() {
    // The steer must land while the first step is in its settle delay, so
    // it queues and applies at the next continuation boundary.
    let provider = SharedLogProvider {
        log: Arc::default(),
        settle_delay: Duration::from_millis(150),
    };
    let runtime = start_runtime(provider.clone(), ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");
    session.steer("and also check tests").await.expect("steer");
    let _ = collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        2,
        "one steer must open exactly one new step"
    );
    assert_eq!(
        requests[0].plan.messages,
        vec![ContextMessage::User {
            content: "goal".to_owned()
        }]
    );
    assert_eq!(
        requests[1].plan.messages,
        vec![
            ContextMessage::User {
                content: "goal".to_owned()
            },
            ContextMessage::Assistant {
                content: "working".to_owned(),
                tool_calls: Vec::new(),
            },
            ContextMessage::User {
                content: "and also check tests".to_owned()
            },
        ],
        "the steer must be projected into the next step's plan"
    );
}

#[tokio::test]
async fn close_while_operating_suspends_instead_of_cancelling() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "late",
        )]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("slow").await.expect("submit");
    sleep(STEP).await;

    session.close().await.expect("close");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    // Close is lifecycle shutdown, never a user cancellation
    // (DESIGN.md §9.5).
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::SessionClosed { .. }))
    );
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationCancelled { .. }))
    );
    runtime.join().await.expect("join");
}
