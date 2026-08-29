use super::support::*;

struct AgentScopeTool {
    name: &'static str,
    description: &'static str,
}

impl Tool for AgentScopeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.to_owned(),
            description: self.description.to_owned(),
            input_schema: json!({"type": "object", "required": []}),
        }
    }

    fn call<'a>(
        &'a self,
        _arguments: serde_json::Value,
        _cancel: CancellationToken,
    ) -> std::pin::Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async { ToolOutcome::text("agent scope probe") })
    }
}

#[tokio::test]
async fn resumed_interactive_recovery_uses_frozen_agents_scope_snapshot() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ModelExecution);
    let catalog = ToolCatalog::default();
    catalog.register_structural_scope(
        "agents",
        vec![Arc::new(AgentScopeTool {
            name: "agent_probe",
            description: "agents-v1",
        })],
    );
    let initial_provider = SharedLogProvider::default();
    let runtime = Runtime::start_interactive_with_effect_gate(
        initial_provider.clone(),
        catalog,
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
        .expect("model gate reached");

    assert!(
        initial_provider.requests().is_empty(),
        "the initial provider must not start before the crash boundary"
    );
    let loaded = store.load(session_id).await.expect("load");
    let frozen = &loaded.operations[0].capability_snapshot;
    assert!(frozen.tools.iter().any(|tool| {
        tool.name == "agent_probe" && tool.description == "agents-v1"
    }));

    runtime.crash();
    gate.release();
    let _ = submit.await.expect("submit task");
    drop(session);
    drop(runtime);

    let live_catalog = ToolCatalog::default();
    live_catalog.register_structural_scope(
        "agents",
        vec![Arc::new(AgentScopeTool {
            name: "agent_probe",
            description: "agents-v2",
        })],
    );
    live_catalog.register_structural_scope(
        "unrelated",
        vec![Arc::new(AgentScopeTool {
            name: "unrelated_probe",
            description: "unrelated",
        })],
    );
    let recovered_provider = SharedLogProvider::default();
    let runtime = Runtime::open_interactive(
        recovered_provider.clone(),
        live_catalog,
        store.clone(),
        session_id,
        permissive_policy(),
        Vec::new(),
    )
    .await
    .expect("reopen interactive");

    // This is the host-attachment window: durable structural authority is
    // restored before the first session command is allowed to trigger recovery.
    runtime
        .admit_structural_scope("agents")
        .await
        .expect("reattach agents scope");

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));

    let requests = recovered_provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].tools.iter().any(|tool| {
        tool.name == "agent_probe" && tool.description == "agents-v1"
    }));
    assert!(!requests[0].tools.iter().any(|tool| {
        tool.name == "agent_probe" && tool.description == "agents-v2"
    }));
    assert!(
        !requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "unrelated_probe")
    );

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
