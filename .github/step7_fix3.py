from pathlib import Path

path = Path("crates/ion-core/src/tests/operation_flow.rs")
text = path.read_text()
old = '''#[tokio::test]
async fn capability_snapshot_refreshes_at_each_model_step() {
    struct DynamicTool;
    impl Tool for DynamicTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "dynamic".to_owned(),
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
    catalog.register_scope("dynamic", vec![Arc::new(DynamicTool)]);
    session.steer("continue").await.expect("steer");
    collect_until_terminal(&mut events).await.expect("collect");
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].tools.iter().any(|tool| tool.name == "dynamic"));
    assert!(requests[1].tools.iter().any(|tool| tool.name == "dynamic"));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
'''
new = '''#[tokio::test]
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
    assert!(requests[0].tools.iter().any(|tool| tool.name == "dynamic-v1"));
    assert!(!requests[0].tools.iter().any(|tool| tool.name == "dynamic-v2"));
    assert!(requests[1].tools.iter().any(|tool| tool.name == "dynamic-v2"));
    assert!(!requests[1].tools.iter().any(|tool| tool.name == "dynamic-v1"));
    assert!(!requests[1].tools.iter().any(|tool| tool.name == "unrelated"));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
'''
if text.count(old) != 1:
    raise SystemExit(f"expected exactly one operation-flow refresh test, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
print("step7 operation-flow regression updated")
