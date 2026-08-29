//! Separately hosted agent runtime lifecycle tests.

use super::support::*;

fn agent_handle(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("agent handle: "))
        .expect("durable agent handle")
        .to_owned()
}

#[tokio::test]
async fn completed_hosted_agents_release_live_runtime_slots() {
    let store = SessionStore::open_in_memory().expect("store");
    let parent_runtime = Runtime::start_with_store(
        ScriptedProvider::new(Vec::new()),
        ToolRegistry::default(),
        store.clone(),
    );
    let family = Arc::new(parent_runtime.agent_family(2).await.expect("family"));
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
        parent_runtime.session_id(),
    );
    let tools = crate::agent_host_tools(Arc::clone(&family), Arc::clone(&hosted));
    let spawn = tools
        .iter()
        .find(|tool| tool.spec().name == "spawn_agent")
        .expect("spawn agent tool");
    let wait = tools
        .iter()
        .find(|tool| tool.spec().name == "agent_wait")
        .expect("wait agent tool");

    let first = spawn
        .call(
            json!({"objective": "first", "topology": "fresh"}),
            CancellationToken::new(),
        )
        .await;
    assert!(!first.is_error, "first agent failed: {first:?}");
    let first_handle = agent_handle(&first.output);
    let waited = wait
        .call(json!({"handle": first_handle}), CancellationToken::new())
        .await;
    assert!(!waited.is_error, "first wait failed: {waited:?}");
    assert!(waited.output.contains("finished"), "{waited:?}");

    let second = spawn
        .call(
            json!({"objective": "second", "topology": "fresh"}),
            CancellationToken::new(),
        )
        .await;
    assert!(
        !second.is_error,
        "a completed hosted agent must not consume the one live slot: {second:?}"
    );
    let second_handle = agent_handle(&second.output);
    let waited = wait
        .call(json!({"handle": second_handle}), CancellationToken::new())
        .await;
    assert!(!waited.is_error, "second wait failed: {waited:?}");

    hosted.close().await.expect("close hosted agents");
    parent_runtime
        .session()
        .close()
        .await
        .expect("close parent");
    parent_runtime.join().await.expect("join parent");
    store.close().await.expect("close store");
}
