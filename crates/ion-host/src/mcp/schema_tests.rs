use super::*;
use ion_ai::ToolCall;
use std::{io::ErrorKind, os::unix::fs::PermissionsExt};

#[test]
fn conformance_and_raw_observation_survive_inline_truncation() {
    let owner = crate::test_support::ArtifactFixture::new();
    let schema = schema::OutputSchema::capture(json!({"type": "array"}));
    let raw = json!({
        "content": [{"type": "text", "text": "server text"}],
        "structuredContent": {"rows": 1, "blob": "é".repeat(40 * 1024)},
        "isError": false,
        "_meta": {"private": "RAW_ONLY"}
    });
    let result = serde_json::from_value(raw.clone()).unwrap();
    let output = conversion::convert_tool_result("wide", result, Some(&schema), Some(owner.sink()));
    assert!(!output.is_error);
    assert_eq!(output.value["is_error"], false);
    assert_eq!(
        output.value["output_schema_validation"]["status"],
        "invalid"
    );
    assert_eq!(output.value["truncated"], true);
    assert_eq!(output.value["structured_content_omitted"], true);
    assert!(output.value["structured_content"].is_null());
    assert!(serde_json::to_vec(&output.value).unwrap().len() < 64 * 1024);
    let raw_index = output.value["raw_result_artifact"].as_u64().unwrap();
    let full_index = output.value["full_output_artifact"].as_u64().unwrap();
    let saved_raw: Value =
        serde_json::from_slice(&owner.bytes(&output.artifacts[raw_index as usize])).unwrap();
    let saved_full: Value =
        serde_json::from_slice(&owner.bytes(&output.artifacts[full_index as usize])).unwrap();
    assert_eq!(saved_raw, raw);
    assert_eq!(saved_full["structured_content"], raw["structuredContent"]);
    assert_eq!(saved_full["output_schema_validation"]["status"], "invalid");
    assert_eq!(saved_full["is_error"], false);
    assert_eq!(
        fs::metadata(owner.session.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn frozen_output_contracts_preserve_results_and_unchecked_tools_across_refresh() {
    let owner = crate::test_support::ArtifactFixture::new();
    let root =
        std::env::temp_dir().join(format!("ion-mcp-schema-capture-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    let calls = root.join("calls");
    let external = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    external.set_nonblocking(true).unwrap();
    let reference = format!("http://{}/SCHEMA_CANARY", external.local_addr().unwrap());
    let server = r#"
import json, sys
version = 1
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request: continue
    method = request['method']
    if method == 'initialize':
        result = {'protocolVersion': request['params']['protocolVersion'],
                  'capabilities': {'tools': {'listChanged': True}},
                  'serverInfo': {'name': 'schema', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [
            {'name': 'probe', 'inputSchema': {'type': 'object'},
             'outputSchema': {'type': 'object' if version == 1 else 'array'}},
            {'name': 'unchecked', 'inputSchema': {'type': 'object'},
             'outputSchema': {'$ref': __EXTERNAL_REF__}}]}
    elif method == 'tools/call':
        name = request['params']['name']
        with open(__CALLS__, 'a') as out: out.write(name + '\n')
        if version == 1:
            version = 2
            print(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/tools/list_changed'}), flush=True)
        result = {'content': [{'type': 'text', 'text': 'remote report'}],
                  'structuredContent': {'called': name, 'value': 'VALUE_CANARY'}, 'isError': False}
    else: continue
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"#.replace("__CALLS__", &serde_json::to_string(&calls).unwrap())
    .replace("__EXTERNAL_REF__", &serde_json::to_string(&reference).unwrap());
    let config = McpConfig::new(&root);
    config
        .add(
            "schema",
            McpServer::Stdio(McpStdioServer {
                enabled: true,
                description: None,
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec!["-c".into(), server],
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup =
        McpTools::connect(&config, &root, tokio_util::sync::CancellationToken::new()).await;
    assert_eq!(startup.diagnostics.len(), 1, "{:?}", startup.diagnostics);
    assert!(startup.diagnostics[0].contains("unchecked"));
    assert!(!startup.diagnostics[0].contains("SCHEMA_CANARY"));
    let source = startup.tools.unwrap();
    let tools = ion_core::ToolSet::new([source.clone() as Arc<dyn ToolSource>]);
    let issued = tools.snapshot();
    assert_eq!(issued.specs().len(), 2, "an unchecked schema hid the tool");
    let call = ToolCall {
        id: "schema".into(),
        name: "mcp__6_schema__probe".into(),
        arguments: json!({}),
        raw_arguments: None,
    };
    let first = issued
        .execute(
            ion_core::ToolInvocation::untracked(&call, CancellationToken::new())
                .with_artifact_sink(owner.sink()),
        )
        .await;
    assert_eq!(first.value["output_schema_validation"]["status"], "valid");
    let refreshed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let diagnostics = tools.refresh(CancellationToken::new()).await;
            assert!(
                diagnostics
                    .iter()
                    .all(|message| message.contains("unchecked")
                        && !message.contains("SCHEMA_CANARY")),
                "{diagnostics:?}"
            );
            let snapshot = tools.snapshot();
            if snapshot
                .definition(&call.name)
                .unwrap()
                .result_schema
                .as_ref()
                .unwrap()
                .schema["type"]
                == "array"
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let old = issued
        .execute(
            ion_core::ToolInvocation::untracked(&call, CancellationToken::new())
                .with_artifact_sink(owner.sink()),
        )
        .await;
    let changed = refreshed
        .execute(
            ion_core::ToolInvocation::untracked(&call, CancellationToken::new())
                .with_artifact_sink(owner.sink()),
        )
        .await;
    let unchecked_call = ToolCall {
        name: "mcp__6_schema__unchecked".into(),
        ..call.clone()
    };
    let unchecked = refreshed
        .execute(
            ion_core::ToolInvocation::untracked(&unchecked_call, CancellationToken::new())
                .with_artifact_sink(owner.sink()),
        )
        .await;
    source.shutdown().await.unwrap();
    assert_eq!(
        old.value["output_schema_validation"]["status"], "valid",
        "old issued schema was rebound"
    );
    assert_eq!(
        changed.value["output_schema_validation"]["status"],
        "invalid"
    );
    assert_eq!(
        unchecked.value["output_schema_validation"]["status"],
        "not_checked"
    );
    for output in [&first, &old, &changed, &unchecked] {
        assert!(!output.is_error);
        assert_eq!(output.value["is_error"], false);
        assert_eq!(output.value["structured_content"]["value"], "VALUE_CANARY");
    }
    for output in [&changed, &unchecked] {
        let raw = owner.json(output, "raw_result_artifact");
        assert!(output.value["raw_result_error"].is_null());
        assert_eq!(
            fs::metadata(owner.session.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(raw["structuredContent"], output.value["structured_content"]);
        assert_eq!(raw["isError"], false);
        assert!(
            raw.get("output_schema_validation").is_none(),
            "local status rewrote raw observation"
        );
    }
    assert_eq!(
        fs::read_to_string(calls).unwrap(),
        "probe\nprobe\nprobe\nunchecked\n",
        "validation replayed a tool call"
    );
    assert!(
        matches!(external.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock),
        "schema resolution fetched a reference"
    );
    fs::remove_dir_all(root).unwrap();
}
