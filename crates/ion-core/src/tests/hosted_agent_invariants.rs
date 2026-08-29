//! Unified hosted-agent topology, capability, budget, and lifecycle invariants.

use super::support::*;

fn host_tool<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> &'a dyn Tool {
    tools
        .iter()
        .find(|tool| tool.spec().name == name)
        .map(AsRef::as_ref)
        .unwrap_or_else(|| panic!("missing host tool {name}"))
}

fn agent_from_output(output: &str) -> crate::AgentId {
    let raw = output
        .lines()
        .find_map(|line| line.strip_prefix("agent handle: "))
        .expect("agent handle");
    crate::AgentId::parse(raw.strip_prefix("agent-").expect("agent prefix")).expect("agent id")
}

fn agent_session(agent_id: crate::AgentId) -> crate::SessionId {
    crate::SessionId::from_uuid(agent_id.as_uuid())
}

async fn spawn(
    tools: &[Arc<dyn Tool>],
    arguments: serde_json::Value,
    cancel: CancellationToken,
) -> ToolOutcome {
    host_tool(tools, "spawn_agent")
        .call(arguments, cancel)
        .await
}

async fn wait(tools: &[Arc<dyn Tool>], agent_id: crate::AgentId) -> ToolOutcome {
    host_tool(tools, "agent_wait")
        .call(
            json!({"handle": agent_id.to_string()}),
            CancellationToken::new(),
        )
        .await
}

#[tokio::test]
async fn hosted_agent_uses_parent_workspace_for_relative_tools() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(
        workspace.path().join("relative.txt"),
        "from parent workspace",
    )
    .expect("write workspace file");
    let store = SessionStore::open_in_memory().expect("store");
    let workspace_text = workspace.path().to_string_lossy().into_owned();
    let runtime = Runtime::start_with_policy_and_resources_in_cwd(
        ScriptedProvider::new(Vec::new()),
        ToolRegistry::default(),
        store.clone(),
        permissive_policy(),
        Vec::new(),
        workspace_text.clone(),
    );
    let family = Arc::new(runtime.agent_family(2).await.expect("family"));
    let hosted = crate::hosted_agent_runtimes(
        crate::HostedAgentConfig {
            store: store.clone(),
            make_provider: Arc::new(|| {
                ScriptedProvider::new(vec![
                    ScriptedMessage::ToolCall {
                        name: "read".to_owned(),
                        arguments: json!({"path": "relative.txt"}),
                    },
                    ScriptedMessage::text("agent completed"),
                ])
            }),
            make_provider_for_model: None,
            max_active: 1,
            budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            cwd: workspace.path().to_path_buf(),
        },
        runtime.session_id(),
    );
    let tools = crate::agent_host_tools(Arc::clone(&family), Arc::clone(&hosted));
    let spawned = spawn(
        &tools,
        json!({"objective": "read the workspace file", "topology": "fresh"}),
        CancellationToken::new(),
    )
    .await;
    assert!(!spawned.is_error, "spawn failed: {spawned:?}");
    let agent_id = agent_from_output(&spawned.output);
    let waited = wait(&tools, agent_id).await;
    assert!(!waited.is_error, "wait failed: {waited:?}");

    let loaded = store
        .load(agent_session(agent_id))
        .await
        .expect("hosted session");
    assert_eq!(loaded.session.cwd, workspace_text);
    let main = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("hosted main lane");
    assert_eq!(main.config.tools, crate::tool::ToolSelection::read_only());
    assert!(loaded.entries.iter().any(|record| {
        serde_json::to_string(&record.entry)
            .map(|text| text.contains("from parent workspace"))
            .unwrap_or(false)
    }));

    hosted.close().await.expect("close hosted agents");
    runtime.session().close().await.expect("close root");
    runtime.join().await.expect("join root");
    store.close().await.expect("close store");
}

#[tokio::test]
async fn unified_host_reports_hosted_agent_lifecycle_progress() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(
        ScriptedProvider::new(Vec::new()),
        ToolRegistry::default(),
        store.clone(),
    );
    let family = Arc::new(runtime.agent_family(2).await.expect("family"));
    let hosted = crate::hosted_agent_runtimes(
        crate::HostedAgentConfig {
            store: store.clone(),
            make_provider: Arc::new(|| {
                ScriptedProvider::new(vec![ScriptedMessage::text("agent answer")])
            }),
            make_provider_for_model: None,
            max_active: 1,
            budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        runtime.session_id(),
    );
    let tools = crate::agent_host_tools(Arc::clone(&family), Arc::clone(&hosted));
    let (progress_tx, mut progress_rx) = mpsc::channel(8);
    let spawned = host_tool(&tools, "spawn_agent")
        .call_with_progress(
            json!({"objective": "research", "topology": "fresh"}),
            CancellationToken::new(),
            Some(progress_tx.clone()),
        )
        .await;
    assert!(!spawned.is_error, "spawn failed: {spawned:?}");
    let agent_id = agent_from_output(&spawned.output);
    let waited = host_tool(&tools, "agent_wait")
        .call_with_progress(
            json!({"handle": agent_id.to_string()}),
            CancellationToken::new(),
            Some(progress_tx),
        )
        .await;
    assert!(!waited.is_error, "wait failed: {waited:?}");
    let mut updates = Vec::new();
    while let Some(update) = progress_rx.recv().await {
        updates.push(update.output);
    }
    assert!(
        updates.iter().any(|update| update.contains("accepted")),
        "missing admission progress: {updates:?}"
    );
    assert!(
        updates.iter().any(|update| update.contains("finished")),
        "missing completion progress: {updates:?}"
    );

    hosted.close().await.expect("close hosted agents");
    runtime.session().close().await.expect("close root");
    runtime.join().await.expect("join root");
    store.close().await.expect("close store");
}

#[tokio::test]
async fn multiple_fresh_agents_are_durable_family_descendants() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(
        ScriptedProvider::new(Vec::new()),
        ToolRegistry::default(),
        store.clone(),
    );
    let parent_id = runtime.session_id();
    let family = Arc::new(runtime.agent_family(4).await.expect("family"));
    let hosted = crate::hosted_agent_runtimes(
        crate::HostedAgentConfig {
            store: store.clone(),
            make_provider: Arc::new(|| {
                ScriptedProvider::new(vec![ScriptedMessage::text("agent answer")])
            }),
            make_provider_for_model: None,
            max_active: 4,
            budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        parent_id,
    );
    let tools = crate::agent_host_tools(Arc::clone(&family), Arc::clone(&hosted));
    let first = spawn(
        &tools,
        json!({"objective": "investigate a", "topology": "fresh"}),
        CancellationToken::new(),
    )
    .await;
    let second = spawn(
        &tools,
        json!({"objective": "investigate b", "topology": "fresh", "context": "seed text"}),
        CancellationToken::new(),
    )
    .await;
    assert!(!first.is_error && !second.is_error, "{first:?} {second:?}");
    let ids = [
        agent_from_output(&first.output),
        agent_from_output(&second.output),
    ];
    for agent_id in ids {
        let waited = wait(&tools, agent_id).await;
        assert!(!waited.is_error, "wait failed: {waited:?}");
        let loaded = store
            .load(agent_session(agent_id))
            .await
            .expect("hosted session");
        assert_eq!(loaded.session.control_parent_session_id, Some(parent_id));
        assert_eq!(loaded.session.fork_source_session_id, None);
        assert_eq!(loaded.session.fork_source_entry_id, None);
        assert_eq!(loaded.agents.len(), 1);
        assert_eq!(loaded.agents[0].id, agent_id);
        assert_eq!(loaded.agents[0].family_session_id, parent_id);
    }
    let second_loaded = store
        .load(agent_session(ids[1]))
        .await
        .expect("second hosted session");
    assert!(second_loaded.entries.iter().any(|record| {
        matches!(
            &record.entry,
            SessionEntry::UserMessage { text } if text.contains("seed text")
        )
    }));

    hosted.close().await.expect("close hosted agents");
    runtime.session().close().await.expect("close root");
    runtime.join().await.expect("join root");
    store.close().await.expect("close store");
}

#[tokio::test]
async fn fork_history_and_model_override_are_explicit() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(
        crate::SwitchingProvider::new(
            "parent-model",
            ScriptedProvider::new(vec![ScriptedMessage::text("parent answer")]),
        ),
        ToolRegistry::default(),
        store.clone(),
    );
    let root_session = runtime.session();
    let (_snapshot, mut events) = root_session.subscribe().await.expect("subscribe");
    root_session
        .submit_if_idle("parent prompt")
        .await
        .expect("submit root prompt");
    collect_until_terminal(&mut events)
        .await
        .expect("root completion");
    let parent_before_spawn = store.load(runtime.session_id()).await.expect("parent load");
    let source_leaf = parent_before_spawn
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("main lane")
        .state
        .leaf;

    let family = Arc::new(runtime.agent_family(2).await.expect("family"));
    let hosted = crate::hosted_agent_runtimes(
        crate::HostedAgentConfig {
            store: store.clone(),
            make_provider: Arc::new(|| {
                crate::SwitchingProvider::new(
                    "default-agent-model",
                    ScriptedProvider::new(vec![ScriptedMessage::text("wrong model")]),
                )
            }),
            make_provider_for_model: Some(Arc::new(|model| {
                crate::SwitchingProvider::new(
                    model,
                    ScriptedProvider::new(vec![ScriptedMessage::text("fork answer")]),
                )
            })),
            max_active: 1,
            budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        runtime.session_id(),
    );
    let tools = crate::agent_host_tools(Arc::clone(&family), Arc::clone(&hosted));
    let spawned = spawn(
        &tools,
        json!({
            "objective": "continue the parent investigation",
            "topology": "fork",
            "model_override": "agent-model"
        }),
        CancellationToken::new(),
    )
    .await;
    assert!(!spawned.is_error, "fork spawn failed: {spawned:?}");
    let agent_id = agent_from_output(&spawned.output);
    let waited = wait(&tools, agent_id).await;
    assert!(!waited.is_error, "fork wait failed: {waited:?}");

    let loaded = store
        .load(agent_session(agent_id))
        .await
        .expect("fork session");
    assert_eq!(
        loaded.session.control_parent_session_id,
        Some(runtime.session_id())
    );
    assert_eq!(
        loaded.session.fork_source_session_id,
        Some(runtime.session_id())
    );
    assert_eq!(loaded.session.fork_source_entry_id, source_leaf);
    assert_eq!(loaded.session.initial_model_ref, "agent-model");
    let prompt = loaded
        .entries
        .iter()
        .find_map(|record| match &record.entry {
            SessionEntry::UserMessage { text } => Some(text),
            _ => None,
        })
        .expect("hosted objective");
    assert!(prompt.contains("continue the parent investigation"));
    assert!(prompt.contains("[Explicit fork of parent semantic context]"));
    assert!(prompt.contains("parent prompt"));
    assert!(prompt.contains("parent answer"));

    hosted.close().await.expect("close hosted agents");
    root_session.close().await.expect("close root");
    runtime.join().await.expect("join root");
    store.close().await.expect("close store");
}

#[tokio::test]
async fn hosted_agent_cannot_widen_read_only_capabilities() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(
        ScriptedProvider::new(Vec::new()),
        ToolRegistry::default(),
        store.clone(),
    );
    let family = Arc::new(runtime.agent_family(2).await.expect("family"));
    let hosted = crate::hosted_agent_runtimes(
        crate::HostedAgentConfig {
            store: store.clone(),
            make_provider: Arc::new(|| {
                ScriptedProvider::new(vec![
                    ScriptedMessage::ToolCall {
                        name: "bash".to_owned(),
                        arguments: json!({"command": "exit 97"}),
                    },
                    ScriptedMessage::text("done"),
                ])
            }),
            make_provider_for_model: None,
            max_active: 1,
            budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        runtime.session_id(),
    );
    let tools = crate::agent_host_tools(Arc::clone(&family), Arc::clone(&hosted));
    let spawned = spawn(
        &tools,
        json!({"objective": "try to escape", "topology": "fresh"}),
        CancellationToken::new(),
    )
    .await;
    assert!(!spawned.is_error, "spawn failed: {spawned:?}");
    let agent_id = agent_from_output(&spawned.output);
    let waited = wait(&tools, agent_id).await;
    assert!(!waited.is_error, "wait failed: {waited:?}");

    let loaded = store
        .load(agent_session(agent_id))
        .await
        .expect("hosted session");
    assert!(loaded.operations.iter().all(|operation| {
        !operation
            .capability_snapshot
            .tools
            .iter()
            .any(|tool| tool.name == "bash")
    }));
    assert!(loaded.entries.iter().any(|record| matches!(
        &record.entry,
        SessionEntry::ToolResult {
            result: ToolResult::Err { error, .. },
        } if error.contains("unknown tool") && error.contains("bash")
    )));

    hosted.close().await.expect("close hosted agents");
    runtime.session().close().await.expect("close root");
    runtime.join().await.expect("join root");
    store.close().await.expect("close store");
}

#[tokio::test]
async fn hosted_agent_budget_stops_runaway_execution() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(
        ScriptedProvider::new(Vec::new()),
        ToolRegistry::default(),
        store.clone(),
    );
    let family = Arc::new(runtime.agent_family(2).await.expect("family"));
    let hosted = crate::hosted_agent_runtimes(
        crate::HostedAgentConfig {
            store: store.clone(),
            make_provider: Arc::new(|| {
                ScriptedProvider::new(vec![
                    ScriptedMessage::ToolCall {
                        name: "read".to_owned(),
                        arguments: json!({"path": "Cargo.toml"}),
                    },
                    ScriptedMessage::ToolCall {
                        name: "read".to_owned(),
                        arguments: json!({"path": "Cargo.toml"}),
                    },
                ])
            }),
            make_provider_for_model: None,
            max_active: 1,
            budget: crate::RuntimeBudget {
                max_model_steps: Some(1),
                max_tool_calls: None,
            },
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        runtime.session_id(),
    );
    let tools = crate::agent_host_tools(Arc::clone(&family), Arc::clone(&hosted));
    let spawned = spawn(
        &tools,
        json!({"objective": "loop", "topology": "fresh"}),
        CancellationToken::new(),
    )
    .await;
    assert!(!spawned.is_error, "spawn failed: {spawned:?}");
    let agent_id = agent_from_output(&spawned.output);
    let waited = wait(&tools, agent_id).await;
    assert!(
        !waited.is_error,
        "durable failed completion is observable: {waited:?}"
    );
    assert!(waited.output.contains("budget"), "{waited:?}");
    let loaded = store
        .load(agent_session(agent_id))
        .await
        .expect("hosted session");
    assert!(loaded.operations.iter().any(|operation| {
        serde_json::to_string(&operation.latest)
            .map(|text| text.contains("budget"))
            .unwrap_or(false)
    }));

    hosted.close().await.expect("close hosted agents");
    runtime.session().close().await.expect("close root");
    runtime.join().await.expect("join root");
    store.close().await.expect("close store");
}

#[derive(Clone)]
struct HangingProvider {
    tokens: Arc<Mutex<Vec<CancellationToken>>>,
}

impl Provider for HangingProvider {
    fn run(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
    ) -> impl Future<Output = ()> + Send {
        self.tokens.lock().expect("tokens").push(cancel.clone());
        async move {
            cancel.cancelled().await;
            let _ = out
                .send(EngineSignal::Cancelled {
                    operation_id: request.operation_id,
                    step: request.step,
                })
                .await;
        }
    }
}

#[tokio::test]
async fn spawn_cancellation_propagates_to_running_hosted_agent() {
    let tokens = Arc::new(Mutex::new(Vec::new()));
    let provider_tokens = Arc::clone(&tokens);
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(
        ScriptedProvider::new(Vec::new()),
        ToolRegistry::default(),
        store.clone(),
    );
    let family = Arc::new(runtime.agent_family(2).await.expect("family"));
    let hosted = crate::hosted_agent_runtimes(
        crate::HostedAgentConfig {
            store: store.clone(),
            make_provider: Arc::new(move || HangingProvider {
                tokens: Arc::clone(&provider_tokens),
            }),
            make_provider_for_model: None,
            max_active: 1,
            budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        runtime.session_id(),
    );
    let tools = crate::agent_host_tools(Arc::clone(&family), Arc::clone(&hosted));
    let spawn_cancel = CancellationToken::new();
    let spawned = spawn(
        &tools,
        json!({"objective": "hang", "topology": "fresh"}),
        spawn_cancel.clone(),
    )
    .await;
    assert!(!spawned.is_error, "spawn failed: {spawned:?}");
    let agent_id = agent_from_output(&spawned.output);
    timeout(Duration::from_secs(2), async {
        loop {
            if !tokens.lock().expect("tokens").is_empty() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("hosted provider started");

    spawn_cancel.cancel();
    timeout(Duration::from_secs(2), async {
        loop {
            if tokens
                .lock()
                .expect("tokens")
                .iter()
                .all(CancellationToken::is_cancelled)
            {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("provider cancellation propagated");
    let status = timeout(
        Duration::from_secs(2),
        family.wait_one(agent_id, CancellationToken::new(), None),
    )
    .await
    .expect("family wait timed out")
    .expect("family wait");
    assert!(matches!(
        status,
        crate::AgentStatus::Finished {
            outcome: OperationOutcome::Cancelled,
            ..
        }
    ));

    hosted.close().await.expect("close hosted agents");
    runtime.session().close().await.expect("close root");
    runtime.join().await.expect("join root");
    store.close().await.expect("close store");
}
