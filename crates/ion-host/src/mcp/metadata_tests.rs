use super::*;
use ion_ai::ToolCall;

#[tokio::test]
async fn cross_server_aliases_keep_both_frozen_routes() {
    let root = std::env::temp_dir().join(format!("ion-mcp-alias-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let config = McpConfig::new(&root);
    let script = r#"
import json, sys
name = sys.argv[1]
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request: continue
    if request["method"] == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}}, "serverInfo": {"name": name, "version": "1"}}
    elif request["method"] == "tools/list":
        result = {"tools": [{"name": name, "inputSchema": {"type": "object"}}]}
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
                    command: "python3".into(),
                    args: vec!["-c".into(), script.into(), tool.into()],
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

#[test]
fn colliding_refresh_keeps_the_complete_previous_catalog() {
    let spec = |name: &str| ToolSpec {
        name: name.into(),
        description: String::new(),
        input_schema: json!({}),
    };
    let catalog = SyncRwLock::new(Catalog::new());
    publish_tools(&catalog, "a", vec![(spec("first"), "original-a".into())]).unwrap();
    publish_tools(&catalog, "b", vec![(spec("second"), "original-b".into())]).unwrap();
    let failed = publish_tools(&catalog, "a", vec![(spec("second"), "changed-a".into())]);
    assert!(failed.is_err());
    let snapshot = catalog.read().unwrap();
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot["a"][0].0.name, "first");
    assert_eq!(snapshot["a"][0].1, "original-a");
    assert_eq!(snapshot["b"][0].0.name, "second");
}
