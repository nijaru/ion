use std::{fs, os::unix::fs::PermissionsExt, sync::Arc};

use ion_ai::ToolCall;
use ion_core::{CodingToolSource, ToolSet};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{McpConfig, McpExposure, McpServer, McpStdioServer, McpTools};

const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: format!("call-{name}"),
        name: name.into(),
        arguments,
        raw_arguments: None,
    }
}

async fn execute(
    tools: &ion_core::ToolCatalog,
    name: &str,
    arguments: Value,
) -> ion_core::CodingToolOutput {
    tools
        .execute_untracked(&call(name, arguments), CancellationToken::new())
        .await
}

fn resource_server() -> String {
    format!(
        r#"
import base64, json, sys
PNG = "{PNG_1X1}"
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {{"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {{"tools": {{}}, "resources": {{"listChanged": True}}}},
                  "serverInfo": {{"name": "resource", "version": "1"}}}}
    elif method == "tools/list":
        result = {{"tools": [{{"name": "probe", "inputSchema": {{"type": "object"}}}}]}}
    elif method == "resources/list":
        cursor = (request.get("params") or {{}}).get("cursor")
        if cursor is None:
            result = {{"resources": [
                {{"uri": "mem://one", "name": "One", "mimeType": "text/plain"}},
                {{"uri": "ui://app", "name": "App", "mimeType": "text/html;profile=mcp-app"}}],
                "nextCursor": "p2"}}
        elif cursor == "p2":
            result = {{"resources": [
                {{"uri": "mem://two", "name": "Two", "mimeType": "application/json", "size": 7}}]}}
        else:
            result = {{"resources": []}}
    elif method == "resources/templates/list":
        result = {{"resourceTemplates": [
            {{"uriTemplate": "mem://{{id}}", "name": "Memory"}},
            {{"uriTemplate": "ui://templated", "name": "App", "mimeType": "text/html; profile=\"mcp-app\""}}]}}
    elif method == "resources/read":
        uri = request["params"]["uri"]
        if uri == "mem://one":
            result = {{"contents": [{{"uri": uri, "mimeType": "text/plain", "text": "hello"}}]}}
        elif uri == "mem://two":
            blob = base64.b64encode(b'{{"a":1}}').decode()
            result = {{"contents": [{{"uri": uri, "mimeType": "application/json", "blob": blob}}]}}
        elif uri == "mem://image":
            result = {{"contents": [{{"uri": uri, "mimeType": "image/png", "blob": PNG}}]}}
        elif uri == "mem://file.bin":
            blob = base64.b64encode(b"\x00\x01binary").decode()
            result = {{"contents": [{{"uri": uri, "mimeType": "application/octet-stream", "blob": blob}}]}}
        elif uri == "mem://large":
            result = {{"contents": [{{"uri": uri, "mimeType": "text/plain", "text": "x" * 70000}}]}}
        elif uri == "mem://empty":
            result = {{"contents": []}}
        else:
            result = {{"contents": []}}
    else:
        continue
    print(json.dumps({{"jsonrpc": "2.0", "id": request["id"], "result": result}}), flush=True)
"#
    )
}

fn broken_resource_server() -> String {
    r#"
import json, sys
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}, "resources": {}},
                  "serverInfo": {"name": "broken", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": []}
    elif method in ("resources/list", "resources/templates/list"):
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"],
                          "error": {"code": -32603, "message": "listing failed"}}), flush=True)
        continue
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#
    .into()
}

#[tokio::test]
async fn resources_list_templates_read_and_capability_gating() {
    let root = std::env::temp_dir().join(format!("ion-mcp-resources-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let config = McpConfig::new(&root);
    config
        .add(
            "resource",
            McpServer::Stdio(McpStdioServer {
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec!["-c".into(), resource_server()],
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    config
        .add(
            "broken",
            McpServer::Stdio(McpStdioServer {
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec!["-c".into(), broken_resource_server()],
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup = McpTools::connect(&config, &root).await;
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    let source = startup.tools.unwrap();
    let tools = ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]).snapshot();
    let names = tools
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    assert!(
        names.iter().any(|name| name == "list_mcp_resources")
            && names
                .iter()
                .any(|name| name == "list_mcp_resource_templates")
            && names.iter().any(|name| name == "read_mcp_resource"),
        "{names:?}"
    );

    let listed = execute(&tools, "list_mcp_resources", json!({})).await;
    assert!(!listed.is_error, "{}", listed.value);
    let resources = listed.value["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 2, "{}", listed.value);
    assert_eq!(resources[0]["server"], "resource");
    assert_eq!(resources[0]["uri"], "mem://one");
    assert_eq!(resources[0]["mimeType"], "text/plain");
    assert_eq!(resources[1]["uri"], "mem://two");
    assert_eq!(resources[1]["size"], 7);
    let errors = listed.value["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{}", listed.value);
    assert_eq!(errors[0]["server"], "broken");
    assert!(
        errors[0]["error"]
            .as_str()
            .unwrap()
            .contains("listing failed"),
        "{}",
        listed.value
    );

    let first_page = execute(&tools, "list_mcp_resources", json!({"server": "resource"})).await;
    assert_eq!(first_page.value["nextCursor"], "p2");
    assert_eq!(
        first_page.value["resources"].as_array().unwrap().len(),
        1,
        "{}",
        first_page.value
    );

    let second_page = execute(
        &tools,
        "list_mcp_resources",
        json!({"server": "resource", "cursor": "p2"}),
    )
    .await;
    assert!(second_page.value.get("nextCursor").is_none());
    assert_eq!(second_page.value["resources"][0]["uri"], "mem://two");

    let cursor_without_server =
        execute(&tools, "list_mcp_resources", json!({"cursor": "p2"})).await;
    assert!(cursor_without_server.is_error);

    let broken = execute(&tools, "list_mcp_resources", json!({"server": "broken"})).await;
    assert!(broken.is_error);
    assert!(
        broken.value["error"]
            .as_str()
            .unwrap()
            .contains("listing failed"),
        "{}",
        broken.value
    );

    let templates = execute(&tools, "list_mcp_resource_templates", json!({})).await;
    assert!(!templates.is_error, "{}", templates.value);
    assert_eq!(
        templates.value["resourceTemplates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        templates.value["resourceTemplates"][0]["uriTemplate"],
        "mem://{id}"
    );

    let text = execute(
        &tools,
        "read_mcp_resource",
        json!({"server": "resource", "uri": "mem://one"}),
    )
    .await;
    assert!(!text.is_error, "{}", text.value);
    assert_eq!(text.value["contents"][0]["text"], "hello");
    assert!(text.images.is_empty());

    let json_blob = execute(
        &tools,
        "read_mcp_resource",
        json!({"server": "resource", "uri": "mem://two"}),
    )
    .await;
    assert!(!json_blob.is_error, "{}", json_blob.value);
    assert_eq!(json_blob.value["contents"][0]["text"], "{\"a\":1}");

    let image = execute(
        &tools,
        "read_mcp_resource",
        json!({"server": "resource", "uri": "mem://image"}),
    )
    .await;
    assert!(!image.is_error, "{}", image.value);
    assert_eq!(image.value["contents"][0]["image"], true);
    assert_eq!(image.images.len(), 1);
    assert_eq!(image.images[0].mime_type().as_str(), "image/png");

    let binary = execute(
        &tools,
        "read_mcp_resource",
        json!({"server": "resource", "uri": "mem://file.bin"}),
    )
    .await;
    assert!(!binary.is_error, "{}", binary.value);
    let saved = binary.value["contents"][0]["savedPath"]
        .as_str()
        .expect("binary resource saved path")
        .to_owned();
    assert!(saved.ends_with(".bin"), "{saved}");
    let metadata = fs::metadata(&saved).unwrap();
    assert_eq!(
        metadata.permissions().mode() & 0o777,
        0o600,
        "private resource capture"
    );
    assert_eq!(fs::read(&saved).unwrap(), b"\x00\x01binary");
    fs::remove_file(&saved).unwrap();

    let large = execute(
        &tools,
        "read_mcp_resource",
        json!({"server": "resource", "uri": "mem://large"}),
    )
    .await;
    assert!(!large.is_error, "{}", large.value);
    assert_eq!(large.value["truncated"], true);
    assert!(large.value["original_bytes"].as_u64().unwrap() > 64 * 1024);
    assert_eq!(large.value["preview"].as_str().unwrap().len(), 4096);
    let full_path = large.value["full_output_path"]
        .as_str()
        .expect("large resource full output path")
        .to_owned();
    let full = super::read_native_snapshot(&full_path).await;
    assert_eq!(full["contents"][0]["text"].as_str().unwrap().len(), 70000);
    fs::remove_file(&full_path).unwrap();

    let invalid_listing = execute(&tools, "list_mcp_resources", json!({"unknown": true})).await;
    assert!(invalid_listing.is_error);
    let invalid_read = execute(&tools, "read_mcp_resource", json!({"server": "resource"})).await;
    assert!(invalid_read.is_error);

    let empty = execute(
        &tools,
        "read_mcp_resource",
        json!({"server": "resource", "uri": "mem://empty"}),
    )
    .await;
    assert!(!empty.is_error, "{}", empty.value);
    assert_eq!(empty.value["contents"], json!([]));
    assert_eq!(empty.value["warnings"][0], "Resource is empty.");

    let missing = execute(
        &tools,
        "read_mcp_resource",
        json!({"server": "missing", "uri": "mem://one"}),
    )
    .await;
    assert!(missing.is_error);
    assert!(
        missing.value["error"]
            .as_str()
            .unwrap()
            .contains("resource servers: broken, resource")
    );

    source.shutdown().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn resource_tools_are_absent_without_resource_capability() {
    let root = std::env::temp_dir().join(format!("ion-mcp-tools-only-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let config = McpConfig::new(&root);
    let server = r#"
import json, sys
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}}, "serverInfo": {"name": "tools", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "probe", "inputSchema": {"type": "object"}}]}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#;
    config
        .add(
            "tools",
            McpServer::Stdio(McpStdioServer {
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec!["-c".into(), server.into()],
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup = McpTools::connect(&config, &root).await;
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    let source = startup.tools.unwrap();
    let tools = ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]).snapshot();
    let names = tools
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    assert!(
        names.iter().all(|name| !name.contains("mcp_resource")),
        "{names:?}"
    );
    source.shutdown().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancelled_resource_read_does_not_wait_for_an_unresponsive_server() {
    let root = std::env::temp_dir().join(format!("ion-mcp-resource-stop-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let config = McpConfig::new(&root);
    let server = r#"
import json, sys, time
from pathlib import Path
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}, "resources": {}},
                  "serverInfo": {"name": "stoppable", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": []}
    elif method == "resources/read":
        Path("requested").touch()
        time.sleep(60)
        continue
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#;
    config
        .add(
            "stoppable",
            McpServer::Stdio(McpStdioServer {
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec!["-c".into(), server.into()],
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup = McpTools::connect(&config, &root).await;
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    let source = startup.tools.unwrap();
    let tools = ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]).snapshot();
    let stop = CancellationToken::new();
    let call = call(
        "read_mcp_resource",
        json!({"server": "stoppable", "uri": "mem://slow"}),
    );
    let task_stop = stop.clone();
    let task = tokio::spawn(async move { tools.execute_untracked(&call, task_stop).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !root.join("requested").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("resource read did not reach the server");
    stop.cancel();
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("cancelled resource read did not settle")
        .unwrap();
    assert!(output.is_error, "{}", output.value);
    let error = output.value["error"].as_str().unwrap();
    assert!(error.contains("cancelled"), "{error}");
    source.shutdown().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn resources_only_server_does_not_require_tools_listing() {
    let root = std::env::temp_dir().join(format!("ion-mcp-resource-only-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let config = McpConfig::new(&root);
    let server = r#"
import json, sys
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"resources": {}},
                  "serverInfo": {"name": "resources-only", "version": "1"}}
    elif method == "tools/list":
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"],
                          "error": {"code": -32601, "message": "tools/list is not supported"}}), flush=True)
        continue
    elif method == "resources/list":
        result = {"resources": [{"uri": "mem://only", "name": "Only", "mimeType": "text/plain"}]}
    elif method == "resources/read":
        result = {"contents": [{"uri": "mem://only", "mimeType": "text/plain", "text": "only"}]}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#;
    config
        .add(
            "resources-only",
            McpServer::Stdio(McpStdioServer {
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec!["-c".into(), server.into()],
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup = McpTools::connect(&config, &root).await;
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    let source = startup.tools.unwrap();
    let tools = ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]).snapshot();
    let listed = execute(&tools, "list_mcp_resources", json!({})).await;
    assert!(!listed.is_error, "{}", listed.value);
    assert_eq!(listed.value["resources"][0]["server"], "resources-only");
    assert_eq!(listed.value["resources"][0]["uri"], "mem://only");
    let text = execute(
        &tools,
        "read_mcp_resource",
        json!({"server": "resources-only", "uri": "mem://only"}),
    )
    .await;
    assert!(!text.is_error, "{}", text.value);
    assert_eq!(text.value["contents"][0]["text"], "only");
    source.shutdown().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn deferred_exposure_requires_discovery_and_hidden_exposure_removes_capabilities() {
    let root = std::env::temp_dir().join(format!("ion-mcp-exposure-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let config = McpConfig::new(&root);
    config
        .add(
            "deferred",
            McpServer::Stdio(McpStdioServer {
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec!["-c".into(), resource_server()],
                exposure: McpExposure::Deferred,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup = McpTools::connect(&config, &root).await;
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    let source = startup.tools.unwrap();
    let tool_set = ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]);
    let snapshot = tool_set.snapshot();
    let deferred_name = snapshot
        .specs()
        .into_iter()
        .find(|spec| spec.name.starts_with("mcp__8_deferred__"))
        .expect("deferred MCP tool")
        .name;
    assert!(!snapshot.is_declared(&deferred_name));
    assert!(snapshot.is_declared("tool_search"));
    assert!(snapshot.is_declared("list_mcp_resources"));
    let deferred_spec = snapshot
        .specs()
        .into_iter()
        .find(|spec| spec.name == deferred_name)
        .unwrap();
    let activated = tool_set.snapshot_with_previous(std::slice::from_ref(&deferred_spec));
    assert!(activated.is_declared(&deferred_name));
    source.shutdown().await.unwrap();
    config.remove("deferred").unwrap();

    let config = McpConfig::new(&root);
    config
        .add(
            "hidden",
            McpServer::Stdio(McpStdioServer {
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec!["-c".into(), resource_server()],
                exposure: McpExposure::Hidden,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup = McpTools::connect(&config, &root).await;
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    let source = startup.tools.unwrap();
    let snapshot = ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]).snapshot();
    let names = snapshot
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    assert!(
        names.iter().all(|name| !name.starts_with("mcp__")),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|name| name.starts_with("list_mcp_")),
        "{names:?}"
    );
    assert!(!names.iter().any(|name| name == "tool_search"), "{names:?}");
    source.shutdown().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}
