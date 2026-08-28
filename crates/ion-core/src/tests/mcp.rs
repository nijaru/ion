//! Mcp tests.

use super::support::*;

// ---- MCP service (DESIGN.md §19) ----

fn fake_mcp_server() -> crate::ServerDef {
    let script = format!(
        "{}/tests/fixtures/fake_mcp_server.py",
        env!("CARGO_MANIFEST_DIR")
    );
    crate::ServerDef {
        name: "fake".to_owned(),
        command: "python3".to_owned(),
        args: vec![script],
    }
}

fn restarting_mcp_server(marker: &std::path::Path) -> crate::ServerDef {
    let script = format!(
        "{}/tests/fixtures/restarting_mcp_server.py",
        env!("CARGO_MANIFEST_DIR")
    );
    crate::ServerDef {
        name: "restarting".to_owned(),
        command: "python3".to_owned(),
        args: vec![script, marker.to_string_lossy().into_owned()],
    }
}

#[tokio::test]
async fn mcp_server_publishes_and_serves_tools_through_the_catalog() {
    let catalog = crate::ToolCatalog::default();
    catalog.activate_mcp_server("fake");
    crate::McpService::new()
        .start_into(&[fake_mcp_server()], &catalog)
        .await;

    // Published under a namespaced scope, visible to model steps.
    let specs = catalog.specs();
    let echo = specs
        .iter()
        .find(|spec| spec.name == "fake__echo")
        .expect("the fake server's echo tool must be registered");
    assert_eq!(echo.description, "Echo the message back");

    // Invocation through the normal Tool contract.
    let outcome = catalog
        .execute(
            "fake__echo",
            &json!({ "message": "hello" }),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.output);
    assert_eq!(outcome.output, "echo: hello");

    // Server-side failures stay model-visible tool errors.
    let outcome = catalog
        .execute(
            "fake__echo",
            &json!({ "message": "hi", "fail": true }),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(outcome.is_error);
    assert!(outcome.output.contains("forced failure"));
    catalog.close().await.expect("catalog close");
    assert!(catalog.get("fake__echo").is_none());
}

#[tokio::test]
async fn mcp_peer_restarts_after_discovery_crash_with_a_bounded_delay() {
    let temp = tempfile::tempdir().expect("tempdir");
    let marker = temp.path().join("restarted");
    let catalog = crate::ToolCatalog::default();
    catalog.activate_mcp_server("restarting");
    crate::McpService::new()
        .start_into(&[restarting_mcp_server(&marker)], &catalog)
        .await;

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut outcome = None;
    while Instant::now() < deadline {
        if marker.exists() && catalog.get("restarting__echo").is_some() {
            let candidate = catalog
                .execute(
                    "restarting__echo",
                    &json!({"message":"after restart"}),
                    CancellationToken::new(),
                )
                .await;
            if !candidate.is_error {
                outcome = Some(candidate);
                break;
            }
        }
        sleep(Duration::from_millis(25)).await;
    }
    let outcome = outcome.expect("the peer must recover after its first discovery crash");
    assert_eq!(outcome.output, "echo: after restart");
}

#[tokio::test]
async fn broken_mcp_server_never_blocks_startup() {
    let catalog = crate::ToolCatalog::default();
    catalog.activate_mcp_server("fake");
    let mut defs = vec![fake_mcp_server()];
    defs.push(crate::ServerDef {
        name: "missing".to_owned(),
        command: "/nonexistent/ion-missing-binary".to_owned(),
        args: vec![],
    });
    crate::McpService::new().start_into(&defs, &catalog).await;
    assert!(
        catalog.specs().iter().any(|s| s.name == "fake__echo"),
        "the healthy server still publishes"
    );
}

#[tokio::test]
async fn mcp_tool_flows_through_the_normal_operation_path() {
    // Catalog with the fake server's tool published.
    let catalog = crate::ToolCatalog::default();
    catalog.activate_mcp_server("fake");
    crate::McpService::new()
        .start_into(&[fake_mcp_server()], &catalog)
        .await;

    // Scripted model: call the MCP tool, then summarize the result.
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "fake__echo".to_owned(),
            arguments: json!({ "message": "through the runtime" }),
        },
        ScriptedMessage::text("done"),
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    // AllowlistPolicy is the documented grant mechanism (§17.2).
    let policy = Arc::new(AllowlistPolicy::new(["fake__echo"]));
    let runtime = Runtime::start_with_policy(provider, catalog, store.clone(), policy);
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "{recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // The remote effect ran through admission/policy/recovery like any
    // native tool and its output reached the next model step.
    let loaded = store.load(session_id).await.expect("load");
    // user -> assistant -> tool call -> tool result -> assistant(done)
    assert_eq!(loaded.entries.len(), 5);
    let last = &loaded.entries.last().expect("entries").entry;
    assert!(
        serde_json::to_string(last)
            .map(|text| text.contains("done"))
            .unwrap_or(false),
        "the continuation step must see the tool output: {last:?}"
    );
}

#[tokio::test]
async fn default_policy_requires_approval_for_mcp_tools() {
    let catalog = crate::ToolCatalog::default();
    catalog.activate_mcp_server("fake");
    crate::McpService::new()
        .start_into(&[fake_mcp_server()], &catalog)
        .await;

    let provider = ScriptedProvider::new(vec![ScriptedMessage::ToolCall {
        name: "fake__echo".to_owned(),
        arguments: json!({ "message": "hi" }),
    }]);
    let store = SessionStore::open_in_memory().expect("store");
    // DefaultPolicy is the runtime default: unbounded remote effects
    // need an explicit grant.
    let runtime = Runtime::start_with_store(provider, catalog, store);
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded.iter().any(
            |e| matches!(e, RuntimeEvent::OperationApprovalRequired { tool, .. }
                if tool == "fake__echo")
        ),
        "unapproved MCP tools terminate with ApprovalRequired semantics: {recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
