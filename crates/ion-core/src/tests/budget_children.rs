//! Budget children tests.

use super::support::*;

// ---- Runtime budgets (§20.5) ----

#[tokio::test]
async fn model_step_budget_fails_the_operation_visibly() {
    // The model keeps requesting tool calls; the budget stops the loop
    // after one step.
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo one" }),
        },
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo two" }),
        },
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_budgeted(
        provider,
        ToolRegistry::default(),
        store.clone(),
        permissive_policy(),
        crate::RuntimeBudget {
            max_model_steps: Some(1),
            max_tool_calls: None,
        },
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded.iter().any(
            |e| matches!(e, RuntimeEvent::OperationFailed { message, .. }
                if message.contains("budget"))
        ),
        "{recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let loaded = store.load(session_id).await.expect("load");
    // The failed operation is durable: it appears in the operation
    // records with a Failed outcome, not just as a live event.
    assert!(
        loaded.operations.iter().any(|op| {
            serde_json::to_string(&op.latest)
                .map(|text| text.contains("budget"))
                .unwrap_or(false)
        }),
        "failed outcome persisted"
    );
}

#[tokio::test]
async fn tool_call_budget_denies_further_tools_model_visibly() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo one" }),
        },
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo two" }),
        },
        ScriptedMessage::text("done"),
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_budgeted(
        provider,
        ToolRegistry::default(),
        store.clone(),
        permissive_policy(),
        crate::RuntimeBudget {
            max_model_steps: None,
            max_tool_calls: Some(1),
        },
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "the model can finish its turn after denials: {recorded:?}"
    );
    let session_id = runtime.session_id();
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Exactly one tool effect was admitted; the second call settled as
    // a model-visible denial.
    let loaded = store.load(session_id).await.expect("load");
    let tool_intents = loaded
        .entries
        .iter()
        .filter(|(_, entry)| {
            serde_json::to_string(entry)
                .map(|text| text.contains("echo two"))
                .unwrap_or(false)
        })
        .count();
    assert_eq!(tool_intents, 1, "second call denied, first admitted");
}

// ---- Bounded child delegation (§20, Step 7) ----

#[tokio::test]
async fn durable_child_handles_support_spawn_status_wait_and_cancel() {
    let store = SessionStore::open_in_memory().expect("store");
    let parent = crate::SessionId::generate();
    let (manager, tools) = crate::child_tools(
        crate::DelegateConfig {
            store: store.clone(),
            make_provider: Arc::new(|| {
                ScriptedProvider::new(vec![ScriptedMessage::text("child answer")])
            }),
            make_provider_for_model: None,
            max_active_children: 4,
            child_budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        parent,
    );

    let spawn = tools[0]
        .call(
            json!({"children": [{"objective": "research the task"}]}),
            CancellationToken::new(),
        )
        .await;
    assert!(!spawn.is_error, "spawn failed: {spawn:?}");
    let handle = spawn
        .output
        .strip_prefix("child handle: ")
        .expect("durable handle")
        .to_owned();

    let status = tools[1]
        .call(json!({"handle": handle}), CancellationToken::new())
        .await;
    assert!(!status.is_error, "status failed: {status:?}");
    assert!(status.output.contains("child session-"), "{status:?}");

    let waited = tools[2]
        .call(json!({"handle": handle}), CancellationToken::new())
        .await;
    assert!(!waited.is_error, "wait failed: {waited:?}");
    assert!(waited.output.contains("finished"), "{waited:?}");
    assert!(waited.output.contains("child answer"), "{waited:?}");

    manager.close().await.expect("close children");
}

fn delegate_tool(
    store: SessionStore,
    child_script: Vec<ScriptedMessage>,
    parent: crate::SessionId,
    budget: crate::RuntimeBudget,
) -> Arc<dyn crate::tool::Tool> {
    Arc::new(crate::DelegateTool::new(
        crate::DelegateConfig {
            store,
            make_provider: Arc::new(move || ScriptedProvider::new(child_script.clone())),
            make_provider_for_model: None,
            max_active_children: 4,
            child_budget: budget,
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        parent,
    ))
}

#[tokio::test]
async fn child_uses_parent_workspace_for_relative_tools() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(
        workspace.path().join("relative.txt"),
        "from parent workspace",
    )
    .expect("write workspace file");
    let child_script = vec![
        ScriptedMessage::ToolCall {
            name: "read".to_owned(),
            arguments: json!({ "path": "relative.txt" }),
        },
        ScriptedMessage::text("child completed"),
    ];
    let parent_provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "delegate".to_owned(),
            arguments: json!({ "children": [{ "objective": "read the workspace file" }] }),
        },
        ScriptedMessage::text("parent completed"),
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = crate::ToolCatalog::with_cwd(workspace.path());
    let workspace_text = workspace.path().to_string_lossy().into_owned();
    let runtime = Runtime::start_with_policy_and_resources_in_cwd(
        parent_provider,
        catalog.clone(),
        store.clone(),
        permissive_policy(),
        Vec::new(),
        workspace_text.clone(),
    );
    let parent_id = runtime.session_id();
    catalog.register_scope(
        "delegate",
        vec![Arc::new(crate::DelegateTool::new(
            crate::DelegateConfig {
                store: store.clone(),
                make_provider: Arc::new(move || ScriptedProvider::new(child_script.clone())),
                make_provider_for_model: None,
                max_active_children: 1,
                child_budget: crate::RuntimeBudget::unbounded(),
                trusted_resources: Vec::new(),
                cwd: workspace.path().to_path_buf(),
            },
            parent_id,
        ))],
    );

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("delegate").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let parent = store.load(parent_id).await.expect("parent session");
    let child_id = parent
        .entries
        .iter()
        .find_map(|(_, entry)| {
            serde_json::to_string(entry)
                .ok()
                .filter(|text| text.contains("child completed"))
                .and_then(|text| child_ids(&text).into_iter().next())
        })
        .expect("child reference");
    let child = store.load(child_id).await.expect("child session");
    assert_eq!(child.session.cwd, workspace_text);
    assert!(child.entries.iter().any(|(_, entry)| {
        serde_json::to_string(entry)
            .map(|text| text.contains("from parent workspace"))
            .unwrap_or(false)
    }));
}

#[tokio::test]
async fn delegate_reports_child_lifecycle_progress() {
    let store = SessionStore::open_in_memory().expect("store");
    let tool = delegate_tool(
        store,
        vec![ScriptedMessage::text("child answer")],
        crate::SessionId::generate(),
        crate::RuntimeBudget::unbounded(),
    );
    let (progress_tx, mut progress_rx) = mpsc::channel(8);
    let outcome = tool
        .call_with_progress(
            json!({ "children": [{ "objective": "research" }] }),
            CancellationToken::new(),
            Some(progress_tx),
        )
        .await;
    let mut updates = Vec::new();
    while let Some(update) = progress_rx.recv().await {
        updates.push(update.output);
    }

    assert!(!outcome.is_error, "child should complete: {outcome:?}");
    assert!(
        updates.iter().any(|update| update.contains("started")),
        "missing child-start progress: {updates:?}"
    );
    assert!(
        updates.iter().any(|update| update.contains("finished")),
        "missing child-finish progress: {updates:?}"
    );
}

/// Extract `session-<uuid>` references from a delegate result.
fn child_ids(output: &str) -> Vec<crate::SessionId> {
    output
        .split("[child session: ")
        .skip(1)
        .filter_map(|part| {
            let end = part.find(']')?;
            crate::ids::SessionId::parse(part[..end].trim_start_matches("session-"))
        })
        .collect()
}

#[tokio::test]
async fn two_read_only_children_run_and_report_lineage() {
    let child_script = vec![ScriptedMessage::text("child answer")];
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "delegate".to_owned(),
            arguments: json!({
                "children": [
                    { "objective": "investigate a" },
                    { "objective": "investigate b", "context": "seed text" }
                ]
            }),
        },
        ScriptedMessage::text("done"),
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = crate::ToolCatalog::default();
    let runtime = Runtime::start_with_store(provider, catalog.clone(), store.clone());
    let parent_id = runtime.session_id();
    catalog.register_scope(
        "delegate",
        vec![delegate_tool(
            store.clone(),
            child_script,
            parent_id,
            crate::RuntimeBudget::unbounded(),
        )],
    );

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("fan out").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "{recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // The tool result reached the parent transcript with both children
    // referenced.
    let loaded = store.load(parent_id).await.expect("load");
    let tool_output = loaded
        .entries
        .iter()
        .find_map(|(_, entry)| {
            serde_json::to_string(entry)
                .ok()
                .filter(|text| text.contains("child answer"))
        })
        .expect("child results in parent transcript");

    // Both children are durable sessions with lineage to the parent.
    let ids = child_ids(&tool_output);
    assert_eq!(ids.len(), 2, "{tool_output}");
    for child in ids {
        let child_loaded = store.load(child).await.expect("child session");
        assert_eq!(child_loaded.session.parent_session_id, Some(parent_id));
    }
}

#[tokio::test]
async fn fork_context_and_model_override_are_explicit() {
    let child_script = vec![ScriptedMessage::text("child answer")];
    let parent_provider = crate::SwitchingProvider::new(
        "parent-model",
        ScriptedProvider::new(vec![
            ScriptedMessage::text("parent answer"),
            ScriptedMessage::tool(
                "delegate",
                json!({
                    "children": [{
                        "objective": "continue the parent investigation",
                        "context_mode": "fork_context",
                        "model_override": "child-model"
                    }]
                }),
            ),
            ScriptedMessage::text("done"),
        ]),
    );
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = crate::ToolCatalog::default();
    let runtime = Runtime::start_with_store(parent_provider, catalog.clone(), store.clone());
    let parent_id = runtime.session_id();
    let override_script = child_script.clone();
    catalog.register_scope(
        "delegate",
        vec![Arc::new(crate::DelegateTool::new(
            crate::DelegateConfig {
                store: store.clone(),
                make_provider: Arc::new(move || {
                    crate::SwitchingProvider::new(
                        "default-child-model",
                        ScriptedProvider::new(vec![ScriptedMessage::text("wrong child model")]),
                    )
                }),
                make_provider_for_model: Some(Arc::new(move |model| {
                    crate::SwitchingProvider::new(
                        model,
                        ScriptedProvider::new(override_script.clone()),
                    )
                })),
                max_active_children: 4,
                child_budget: crate::RuntimeBudget::unbounded(),
                trusted_resources: Vec::new(),
                cwd: std::env::current_dir().expect("cwd"),
            },
            parent_id,
        ))],
    );

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session
        .submit_if_idle("parent prompt")
        .await
        .expect("submit");
    collect_until_terminal(&mut events)
        .await
        .expect("parent turn");
    session
        .submit_if_idle("delegate with a fork")
        .await
        .expect("submit");
    collect_until_terminal(&mut events)
        .await
        .expect("delegate turn");
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let parent = store.load(parent_id).await.expect("parent session");
    let child_id = parent
        .entries
        .iter()
        .find_map(|(_, entry)| {
            serde_json::to_string(entry)
                .ok()
                .filter(|text| text.contains("child answer"))
                .and_then(|text| child_ids(&text).into_iter().next())
        })
        .expect("fork child reference");
    let child = store.load(child_id).await.expect("child session");
    assert_eq!(child.session.initial_model_ref, "child-model");
    let prompt = child
        .entries
        .iter()
        .find_map(|(_, entry)| match entry {
            crate::SessionEntry::UserMessage { text } => Some(text),
            _ => None,
        })
        .expect("child objective is durable");
    assert!(prompt.contains("continue the parent investigation"));
    assert!(prompt.contains("[Explicit fork of parent semantic context]"));
    assert!(prompt.contains("parent prompt"));
    assert!(prompt.contains("parent answer"));
}

#[tokio::test]
async fn child_cannot_widen_capabilities() {
    // The child's provider asks for bash; the read-only catalog has no
    // bash and the gate denies the unknown tool model-visibly.
    let child_script = vec![ScriptedMessage::ToolCall {
        name: "bash".to_owned(),
        arguments: json!({ "command": "rm -rf /" }),
    }];
    let provider = ScriptedProvider::new(vec![ScriptedMessage::ToolCall {
        name: "delegate".to_owned(),
        arguments: json!({ "children": [{ "objective": "try to escape" }] }),
    }]);
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = crate::ToolCatalog::default();
    let runtime = Runtime::start_with_store(provider, catalog.clone(), store.clone());
    let parent_id = runtime.session_id();
    catalog.register_scope(
        "delegate",
        vec![delegate_tool(
            store.clone(),
            child_script,
            parent_id,
            crate::RuntimeBudget::unbounded(),
        )],
    );

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("escape").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "{recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let loaded = store.load(parent_id).await.expect("load");
    let tool_output = loaded
        .entries
        .iter()
        .find_map(|(_, entry)| {
            serde_json::to_string(entry)
                .ok()
                .filter(|text| text.contains("child failed"))
        })
        .expect("the escape attempt fails visibly");
    assert!(
        tool_output.contains("unknown tool") || tool_output.contains("approval"),
        "denial is about the capability, not a crash: {tool_output}"
    );
}

#[tokio::test]
async fn budget_stops_a_runaway_child() {
    // The child would loop forever; its budget stops it after one
    // model step. The parent still finishes.
    let looping_child = || {
        ScriptedProvider::new(vec![
            ScriptedMessage::ToolCall {
                name: "read".to_owned(),
                arguments: json!({ "path": "Cargo.toml" }),
            },
            ScriptedMessage::ToolCall {
                name: "read".to_owned(),
                arguments: json!({ "path": "Cargo.toml" }),
            },
        ])
    };
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "delegate".to_owned(),
            arguments: json!({ "children": [{ "objective": "loop" }] }),
        },
        ScriptedMessage::text("gave up on the child"),
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = crate::ToolCatalog::default();
    let runtime = Runtime::start_with_store(provider, catalog.clone(), store.clone());
    let parent_id = runtime.session_id();
    catalog.register_scope(
        "delegate",
        vec![Arc::new(crate::DelegateTool::new(
            crate::DelegateConfig {
                store: store.clone(),
                make_provider: Arc::new(looping_child),
                make_provider_for_model: None,
                max_active_children: 4,
                child_budget: crate::RuntimeBudget {
                    max_model_steps: Some(1),
                    max_tool_calls: None,
                },
                trusted_resources: Vec::new(),
                cwd: std::env::current_dir().expect("cwd"),
            },
            parent_id,
        ))],
    );

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("run away").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "parent survives a failed child: {recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let loaded = store.load(parent_id).await.expect("load");
    let tool_output = loaded
        .entries
        .iter()
        .find_map(|(_, entry)| {
            serde_json::to_string(entry)
                .ok()
                .filter(|text| text.contains("budget exceeded"))
        })
        .expect("budget failure surfaces to the parent");
    assert!(tool_output.contains("child session"));
}

/// A provider that records the cancellation tokens it is given and
/// then waits for cancellation - a child that hangs forever unless the
/// parent's cancel propagates (§20.6).
#[derive(Clone)]
struct HangingProvider {
    tokens: Arc<std::sync::Mutex<Vec<tokio_util::sync::CancellationToken>>>,
}

impl crate::Provider for HangingProvider {
    fn run(
        &self,
        request: crate::ProviderRequest,
        cancel: tokio_util::sync::CancellationToken,
        out: tokio::sync::mpsc::Sender<crate::EngineSignal>,
    ) -> impl Future<Output = ()> + Send {
        self.tokens.lock().expect("tokens").push(cancel.clone());
        async move {
            cancel.cancelled().await;
            let _ = out
                .send(crate::EngineSignal::Cancelled {
                    operation_id: request.operation_id,
                    step: request.step,
                })
                .await;
        }
    }
}

#[tokio::test]
async fn parent_cancel_cancels_running_children() {
    let tokens: Arc<std::sync::Mutex<Vec<tokio_util::sync::CancellationToken>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let spy_tokens = Arc::clone(&tokens);
    let provider = ScriptedProvider::new(vec![ScriptedMessage::ToolCall {
        name: "delegate".to_owned(),
        arguments: json!({ "children": [{ "objective": "hang" }] }),
    }]);
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = crate::ToolCatalog::default();
    let runtime = Runtime::start_with_store(provider, catalog.clone(), store.clone());
    let parent_id = runtime.session_id();
    catalog.register_scope(
        "delegate",
        vec![Arc::new(crate::DelegateTool::new(
            crate::DelegateConfig {
                store: store.clone(),
                make_provider: Arc::new(move || HangingProvider {
                    tokens: Arc::clone(&spy_tokens),
                }),
                make_provider_for_model: None,
                max_active_children: 4,
                child_budget: crate::RuntimeBudget::unbounded(),
                trusted_resources: Vec::new(),
                cwd: std::env::current_dir().expect("cwd"),
            },
            parent_id,
        ))],
    );

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("cancel me").await.expect("submit");
    // Wait for the child provider to record its token, then cancel.
    for _ in 0..100 {
        if !tokens.lock().expect("tokens").is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !tokens.lock().expect("tokens").is_empty(),
        "child provider started"
    );
    session.cancel(operation_id).await.expect("cancel");

    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationCancelled { .. })),
        "{recorded:?}"
    );
    // §20.6: the descendant's token fired with the parent's.
    for _ in 0..100 {
        if tokens
            .lock()
            .expect("tokens")
            .iter()
            .all(|token| token.is_cancelled())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        tokens
            .lock()
            .expect("tokens")
            .iter()
            .all(|token| token.is_cancelled()),
        "parent cancellation must reach descendants"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
