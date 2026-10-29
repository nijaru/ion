use super::*;
use ion_ai::ToolCall;

#[tokio::test]
async fn cross_server_aliases_and_descriptions_keep_frozen_routes_across_refresh() {
    let root = std::env::temp_dir().join(format!("ion-mcp-alias-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let config = McpConfig::new(&root);
    let script = r#"
import json, sys
name = sys.argv[1]
listings = 0
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request: continue
    if request["method"] == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}}, "serverInfo": {"name": name, "version": "1"}}
    elif request["method"] == "tools/list":
        listings += 1
        result = {"tools": [{"name": name if listings == 1 else name + "_next", "description": "Remote tool", "inputSchema": {"type": "object"}}]}
    elif request["method"] == "tools/call":
        result = {"content": [], "structuredContent": {"called": request["params"]["name"]}}
    else: continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#;
    for (server, tool) in [("a", "b__c"), ("a__b", "c")] {
        config
            .add(
                server,
                McpServer::Stdio(McpStdioServer {
                    enabled: true,
                    description: Some(
                        if server == "a" {
                            "  Catalog purpose  "
                        } else {
                            " \n "
                        }
                        .into(),
                    ),
                    cwd: None,
                    env: Default::default(),
                    command: "python3".into(),
                    args: vec!["-c".into(), script.into(), tool.into()],
                    exposure: McpExposure::Direct,
                    tool_exposure: Default::default(),
                }),
            )
            .unwrap();
    }
    let startup = McpTools::connect(&config, &root).await;
    let source = startup.tools.unwrap();
    let issued = ion_core::ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]).snapshot();
    let mut observed = Vec::new();
    for spec in issued.specs() {
        observed.push(
            issued
                .execute_untracked(
                    &ToolCall {
                        id: spec.name.clone(),
                        name: spec.name.clone(),
                        arguments: json!({}),
                        raw_arguments: None,
                    },
                    CancellationToken::new(),
                )
                .await,
        );
    }
    // Saved edits do not retarget an active connection, even at catalogue refresh.
    for (name, mut server) in config.list().unwrap().servers {
        match &mut server {
            McpServer::Stdio(server) => server.description = Some("Replacement purpose".into()),
            McpServer::Http(_) => unreachable!(),
        }
        config.remove(&name).unwrap();
        config.add(&name, server).unwrap();
    }
    for server in &source.servers {
        server.tool_list_version.fetch_add(1, Ordering::AcqRel);
    }
    assert!(source.refresh(CancellationToken::new()).await.is_empty());
    let refreshed =
        ion_core::ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]).snapshot();
    for (server, original) in [("a", "b__c"), ("a__b", "c")] {
        let expected = if server == "a" {
            "Remote tool\n\nMCP server a: Catalog purpose"
        } else {
            "Remote tool"
        };
        let original_name = exposed_tool_name(server, original);
        let next_name = exposed_tool_name(server, &format!("{original}_next"));
        assert_eq!(
            issued.definition(&original_name).unwrap().spec.description,
            expected
        );
        assert!(refreshed.definition(&original_name).is_none());
        assert_eq!(
            refreshed.definition(&next_name).unwrap().spec.description,
            expected
        );
    }
    source.shutdown().await.unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    assert_eq!(
        issued.specs().len(),
        2,
        "one MCP route silently hid another"
    );
    assert!(observed.iter().all(|result| !result.is_error));
    let mut originals = observed
        .iter()
        .map(|result| {
            result.value["structured_content"]["called"]
                .as_str()
                .unwrap()
        })
        .collect::<Vec<_>>();
    originals.sort_unstable();
    assert_eq!(originals, ["b__c", "c"]);
}

#[tokio::test]
async fn exact_tool_overrides_apply_before_aliasing_and_preserve_server_defaults() {
    let root = std::env::temp_dir().join(format!("ion-mcp-exposure-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let config = McpConfig::new(&root);
    let script = r#"
import json, sys
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request: continue
    if request["method"] == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}, "resources": {}},
                  "serverInfo": {"name": "exposure", "version": "1"}}
    elif request["method"] == "tools/list":
        result = {"tools": [{"name": name, "inputSchema": {"type": "object"}}
                            for name in ["read.document", "search/document", "remove", "fallback", "family.read", "family.write", "family?literal"]]}
    elif request["method"] == "resources/read":
        result = {"contents": [{"uri": request["params"]["uri"], "text": "secret"}]}
    elif request["method"] == "tools/call":
        result = {"content": [], "structuredContent": {"called": request["params"]["name"]}}
    else: continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#;
    for (name, exposure) in [
        ("direct", McpExposure::Direct),
        ("deferred", McpExposure::Deferred),
        ("hidden", McpExposure::Hidden),
    ] {
        config
            .add(
                name,
                McpServer::Stdio(McpStdioServer {
                    enabled: true,
                    description: Some(format!("  {name} server purpose  ")),
                    cwd: None,
                    env: Default::default(),
                    command: "python3".into(),
                    args: vec!["-c".into(), script.into()],
                    exposure,
                    tool_exposure: IndexMap::from([
                        ("family.*".into(), McpExposure::Deferred),
                        ("family*".into(), McpExposure::Hidden),
                        ("read.*".into(), McpExposure::Hidden),
                        ("read.document".into(), McpExposure::Direct),
                        ("search/*".into(), McpExposure::Direct),
                        ("search/document".into(), McpExposure::Deferred),
                        ("remove".into(), McpExposure::Hidden),
                        // Patterns and exact keys both use original names, not aliases.
                        (exposed_tool_name(name, "fallback"), McpExposure::Hidden),
                    ]),
                }),
            )
            .unwrap();
    }
    let startup = McpTools::connect(&config, &root).await;
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    let source = startup.tools.unwrap();
    let snapshot = ion_core::ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]).snapshot();
    for (server, fallback) in [
        ("direct", Some(ToolExposure::Direct)),
        ("deferred", Some(ToolExposure::Deferred)),
        ("hidden", None),
    ] {
        for (original, expected) in [
            ("read.document", Some(ToolExposure::Direct)),
            ("search/document", Some(ToolExposure::Deferred)),
            ("remove", None),
            ("fallback", fallback),
            ("family.read", Some(ToolExposure::Deferred)),
            ("family.write", Some(ToolExposure::Deferred)),
            ("family?literal", None),
        ] {
            let name = exposed_tool_name(server, original);
            assert_eq!(
                snapshot
                    .definition(&name)
                    .map(|definition| definition.exposure),
                expected,
                "{server}: {original}"
            );
            assert_eq!(
                snapshot.is_declared(&name),
                expected == Some(ToolExposure::Direct)
            );
            let result = snapshot
                .execute_untracked(
                    &ToolCall {
                        id: name.clone(),
                        name,
                        arguments: json!({}),
                        raw_arguments: None,
                    },
                    CancellationToken::new(),
                )
                .await;
            assert_eq!(result.is_error, expected.is_none(), "{}", result.value);
            if expected.is_some() {
                let definition = snapshot
                    .definition(&exposed_tool_name(server, original))
                    .unwrap();
                assert_eq!(
                    definition.spec.description,
                    format!(
                        "Tool {original} from MCP server {server}\n\nMCP server {server}: {server} server purpose"
                    )
                );
                assert_eq!(result.value["structured_content"]["called"], original);
            }
        }
    }
    let resources = snapshot
        .execute_untracked(
            &ToolCall {
                id: "hidden-resource".into(),
                name: "read_mcp_resource".into(),
                arguments: json!({"server": "hidden", "uri": "mem://secret"}),
                raw_arguments: None,
            },
            CancellationToken::new(),
        )
        .await;
    assert!(
        resources.is_error,
        "tool overrides must not expose hidden server resources"
    );
    source.shutdown().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn colliding_refresh_keeps_the_complete_previous_catalog() {
    let spec = |name: &str| ToolSpec {
        name: name.into(),
        description: String::new(),
        input_schema: json!({}),
    };
    let catalog = SyncRwLock::new(Catalog::new());
    let registered = |spec, original_name: &str, schema| RegisteredTool {
        spec,
        original_name: original_name.into(),
        result_schema: Some(ToolResultSchema {
            value_path: vec!["structured_content".into()],
            schema,
        }),
    };
    publish_tools(
        &catalog,
        "a",
        vec![registered(
            spec("first"),
            "original-a",
            json!({"type":"string"}),
        )],
    )
    .unwrap();
    publish_tools(
        &catalog,
        "b",
        vec![registered(
            spec("second"),
            "original-b",
            json!({"type":"boolean"}),
        )],
    )
    .unwrap();
    let failed = publish_tools(
        &catalog,
        "a",
        vec![registered(
            spec("second"),
            "changed-a",
            json!({"type":"number"}),
        )],
    );
    assert!(failed.is_err());
    let snapshot = catalog.read().unwrap();
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot["a"][0].spec.name, "first");
    assert_eq!(snapshot["a"][0].original_name, "original-a");
    assert_eq!(
        snapshot["a"][0].result_schema.as_ref().unwrap().schema,
        json!({"type":"string"})
    );
    assert_eq!(snapshot["b"][0].spec.name, "second");
}
