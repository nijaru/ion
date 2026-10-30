//! Explicit MCP tool connections. Session facts remain in ion-core.
use std::sync::{
    Arc, RwLock as SyncRwLock,
    atomic::{AtomicU64, Ordering},
};

use indexmap::IndexMap;
use ion_core::CodingToolOutput;
use rmcp::{
    ClientHandler, RoleClient,
    service::{NotificationContext, RunningService},
};
use serde_json::json;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
#[cfg(test)]
use {
    futures_util::future::join_all,
    ion_core::{CodingToolSource, ToolResultSchema},
    serde_json::Value,
    std::{fs, time::Duration},
};

mod artifact;
mod catalog;
mod config;
mod connection;
mod conversion;
mod framing;
mod http;
mod http_config;
mod invocation;
mod launch;
mod manager;
mod request;
mod resource_content;
mod resource_tools;
mod schema;
mod send;
mod server;
mod stderr;
mod value_command;
mod values;
use catalog::Catalog;
#[cfg(test)]
use catalog::exposed_tool_name;
use connection::Connection;
pub use values::McpValue;

#[cfg(test)]
mod invocation_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod metadata_tests;
#[cfg(test)]
mod reconnect_tests;
#[cfg(test)]
mod resource_tests;
#[cfg(test)]
mod schema_tests;

pub use config::{McpConfig, McpExposure, McpHttpServer, McpListing, McpServer, McpStdioServer};

#[derive(Clone)]
struct ChangeHandler(Arc<AtomicU64>);

impl ClientHandler for ChangeHandler {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

type Client = RunningService<RoleClient, ChangeHandler>;
struct Server {
    name: String,
    description: Option<String>,
    exposure: McpExposure,
    tool_exposure: IndexMap<String, McpExposure>,
    /// The initialize handshake advertised resource operations. Capabilities are
    /// fixed for the connection, so callers do not need a second discovery round.
    resources: bool,
    client: Arc<RwLock<Option<Connection>>>,
    stderr: Option<Arc<stderr::Log>>,
    catalog: Arc<SyncRwLock<Catalog>>,
    tool_list_version: Arc<AtomicU64>,
    refreshed_version: AtomicU64,
    retiring: CancellationToken,
    retirement: Mutex<Option<server::Retirement>>,
}

pub use manager::{
    McpReconnect, McpReconnectReport, McpServerSnapshot, McpServerStatus, McpStartup,
    McpToolCounts, McpTools,
};

fn tool_error(message: String) -> CodingToolOutput {
    CodingToolOutput {
        value: json!({"error":message}),
        images: Vec::new(),
        is_error: true,
    }
}

#[cfg(test)]
async fn read_native_snapshot(path: &str) -> Value {
    let source: Arc<dyn CodingToolSource> =
        Arc::new(crate::local_tools::LocalTools::new(std::env::temp_dir()).unwrap());
    let catalog = ion_core::ToolSet::new([source])
        .with_selection(ion_core::ToolSelection::include(["read".into()]))
        .snapshot();
    let mut encoded = String::new();
    let mut offset = 0;
    loop {
        let call = ion_ai::ToolCall {
            id: "snapshot-page".into(),
            name: "read".into(),
            arguments: json!({"path":path,"byte_offset":offset}),
            raw_arguments: None,
        };
        let output = catalog
            .execute_untracked(&call, CancellationToken::new())
            .await;
        if output.is_error {
            fs::remove_file(path).unwrap();
        }
        assert!(!output.is_error, "{}", output.value);
        let content = output.value["content"].as_str().unwrap();
        assert!(content.len() <= 64 * 1024);
        encoded.push_str(content);
        if !output.value["has_more"].as_bool().unwrap() {
            break;
        }
        let next = output.value["next_byte"].as_u64().unwrap();
        assert!(next > offset, "pagination must make progress");
        offset = next;
    }
    serde_json::from_str(&encoded).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use conversion::convert_tool_result;
    use ion_ai::ToolCall;
    use rmcp::model::{CallToolResult, ContentBlock};
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn issued_mcp_capability_survives_peer_refresh_but_not_connection_shutdown() {
        let root = std::env::temp_dir().join(format!("ion-mcp-bound-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let config = McpConfig::new(&root);
        let server = r#"
import json, sys
listings = 0
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}}, "serverInfo": {"name": "bound", "version": "1"}}
    elif method == "tools/list":
        listings += 1
        result = {"tools": [{"name": "probe", "inputSchema": {"type": "object"},
                             "outputSchema": {"type": "object", "properties": {"called": {"type": "string"}}}}]
                  if listings == 1 else []}
    elif method == "tools/call":
        result = {"content": [], "structuredContent": {"called": request["params"]["name"]}}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
"#;
        config
            .add(
                "bound",
                McpServer::Stdio(McpStdioServer {
                    enabled: true,
                    description: None,
                    cwd: None,
                    env: Default::default(),
                    command: "python3".into(),
                    args: vec!["-c".into(), server.into()],
                    exposure: McpExposure::Direct,
                    tool_exposure: Default::default(),
                }),
            )
            .unwrap();
        let startup =
            McpTools::connect(&config, &root, tokio_util::sync::CancellationToken::new()).await;
        assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
        let source = startup.tools.unwrap();
        let tools = ion_core::ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]);
        let issued = tools.snapshot();

        // Persisted startup policy cannot revoke an active or issued capability.
        let project = McpConfig::with_project(&root, &root).unwrap();
        project.set_project_enabled("bound", false).unwrap();
        project.trust_project().unwrap();
        let disabled =
            McpTools::connect(&project, &root, tokio_util::sync::CancellationToken::new()).await;
        assert!(disabled.tools.is_none());
        assert!(
            disabled.diagnostics.is_empty(),
            "{:?}",
            disabled.diagnostics
        );

        // Another consumer refreshes the source after this request was issued.
        source.servers()[0]
            .tool_list_version
            .fetch_add(1, Ordering::AcqRel);
        let diagnostics = tools.refresh(CancellationToken::new()).await;
        let refreshed = tools.snapshot();
        let call = ToolCall {
            id: "issued".into(),
            name: "mcp__5_bound__probe".into(),
            arguments: json!({}),
            raw_arguments: None,
        };
        let observed = issued
            .execute_untracked(&call, CancellationToken::new())
            .await;
        source.shutdown().await.unwrap();
        let closed = issued
            .execute_untracked(&call, CancellationToken::new())
            .await;
        fs::remove_dir_all(root).unwrap();

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(refreshed.specs().is_empty());
        assert_eq!(
            issued.definition(&call.name).unwrap().result_schema,
            Some(ToolResultSchema {
                value_path: vec!["structured_content".into()],
                schema: json!({"type":"object","properties":{"called":{"type":"string"}}}),
            })
        );
        assert!(!observed.is_error, "{}", observed.value);
        assert_eq!(observed.value["structured_content"]["called"], "probe");
        assert!(closed.is_error);
        assert_eq!(closed.value["error"], "MCP server bound is closed");
    }

    #[test]
    fn observed_results_wait_for_workers_through_cancellation_and_connection_shutdown() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            for (name, arguments, method, saved_field) in [
                ("mcp__10_settlement__probe", json!({}), "tools/call", "/content"),
                ("read_mcp_resource", json!({"server":"settlement", "uri":"mem://observed"}), "resources/read", "/contents/0/text"),
                ("list_mcp_resources", json!({"server":"settlement"}), "resources/list", "/resources/0/name"),
                ("list_mcp_resource_templates", json!({"server":"settlement"}), "resources/templates/list", "/resourceTemplates/0/name"),
                ("list_mcp_resources", json!({}), "resources/list", "/resources/0/name"),
            ] {
                let root = std::env::temp_dir().join(format!("ion-mcp-settlement-{}", uuid::Uuid::now_v7()));
                fs::create_dir(&root).unwrap();
                let config = McpConfig::new(&root);
                let server = r#"
import json, sys
from pathlib import Path
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {"listChanged": True}, "resources": {}},
                  "serverInfo": {"name": "settlement", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "probe", "inputSchema": {"type": "object"}}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": "x" * 70000}],
                  "structuredContent": {"observed": 7}, "isError": True}
    elif method == "resources/read":
        result = {"contents": [{"uri": "mem://observed", "mimeType": "text/plain", "text": "x" * 70000}]}
    elif method == "resources/list":
        result = {"resources": [{"uri": "mem://observed", "name": "x" * 70000}]}
    elif method == "resources/templates/list":
        result = {"resourceTemplates": [{"uriTemplate": "mem://{id}", "name": "x" * 70000}]}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
    if method in ("tools/call", "resources/read", "resources/list", "resources/templates/list"):
        with Path("requests").open("a") as requests:
            requests.write(method + "\n")
        print(json.dumps({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}), flush=True)
"#;
                config.add("settlement", McpServer::Stdio(McpStdioServer {
                    enabled: true,
                    description: None,
                    cwd: None,
                    env: Default::default(),
                    command: "python3".into(), args: vec!["-c".into(), server.into()],
                    exposure: McpExposure::Direct,
                    tool_exposure: Default::default(),
                })).unwrap();
                let startup = McpTools::connect(&config, &root, tokio_util::sync::CancellationToken::new()).await;
                assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
                let source = startup.tools.unwrap();
                let issued = ion_core::ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]).snapshot();
                let (entered, started) = tokio::sync::oneshot::channel();
                let (release, held) = tokio::sync::oneshot::channel::<()>();
                let blocker = tokio::task::spawn_blocking(move || {
                    let _ = entered.send(());
                    let _ = held.blocking_recv();
                });
                started.await.unwrap();
                let stop = CancellationToken::new();
                let task_stop = stop.clone();
                let mut work = tokio::spawn(async move {
                    issued.execute_untracked(&ToolCall {
                        id: "observed".into(), name: name.into(),
                        arguments, raw_arguments: None,
                    }, task_stop).await
                });
                // The ordered notification follows routing of the response to
                // its request handle. Conversion must release the connection
                // but remain pending while the only blocking worker is held.
                let observed = tokio::time::timeout(Duration::from_secs(2), async {
                    while source.servers()[0].tool_list_version.load(Ordering::Acquire) == 0 {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                }).await;
                let closing = source.shutdown();
                tokio::pin!(closing);
                let closed = tokio::time::timeout(Duration::from_secs(4), &mut closing).await;
                let independent = closed.is_ok();
                let early = tokio::time::timeout(Duration::from_millis(10), &mut work).await.ok();
                let pending = early.is_none();
                stop.cancel();
                // Poll the cancelled invocation while conversion is still
                // blocked; checking is_finished immediately would not yield.
                let after_stop = if pending {
                    tokio::time::timeout(Duration::from_millis(10), &mut work).await.ok()
                } else {
                    None
                };
                let still_pending = pending && after_stop.is_none();
                drop(release);
                blocker.await.unwrap();
                // Rescue a timed-out close only after releasing conversion;
                // never drop a partial cleanup join on a failing assertion.
                let cleanup = match closed {
                    Ok(result) => result,
                    Err(_) => tokio::time::timeout(Duration::from_secs(4), &mut closing)
                        .await.expect("conversion release did not unblock cleanup"),
                };
                let output = match early.or(after_stop) {
                    Some(output) => output.unwrap(),
                    None => work.await.unwrap(),
                };
                let requests = fs::read_to_string(root.join("requests")).unwrap();
                fs::remove_dir_all(root).unwrap();
                let path = output.value["full_output_path"].as_str().unwrap();
                let saved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
                fs::remove_file(path).unwrap();
                assert!(observed.is_ok(), "{name}: peer did not observe the ordered response");
                cleanup.unwrap();
                assert!(independent, "{name}: conversion held the MCP connection");
                assert!(pending && still_pending, "{name}: result bypassed owned blocking dispatch");
                assert_eq!(requests, format!("{method}\n"));
                assert_eq!(output.is_error, method == "tools/call", "{name}: observed outcome changed during cancellation");
                assert_eq!(saved.pointer(saved_field).unwrap(), &json!("x".repeat(70000)));
                if method == "tools/call" {
                    assert_eq!(saved["structured_content"]["observed"], 7);
                }
            }
        });
    }

    #[test]
    fn model_facing_mcp_names_preserve_distinct_originals() {
        let plain = exposed_tool_name("demo", "admin_tools_list");
        let dotted = exposed_tool_name("demo", "admin.tools.list");
        let long = exposed_tool_name("demo", &"query".repeat(30));
        let other_server = exposed_tool_name("other", "admin.tools.list");
        assert_eq!(plain, "mcp__4_demo__admin_tools_list");
        assert!(dotted.starts_with("mcp__4_demo__admin_tools_list_"));
        assert_ne!(dotted, plain);
        assert_ne!(dotted, other_server);
        assert!(long.len() <= 64);
        assert_eq!(dotted, exposed_tool_name("demo", "admin.tools.list"));
        assert!(
            dotted
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        );
    }

    #[tokio::test]
    async fn malformed_mcp_config_does_not_block_coding_startup() {
        let root = std::env::temp_dir().join(format!("ion-mcp-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("mcp.json"), b"{invalid").unwrap();
        let startup = McpTools::connect(
            &McpConfig::new(&root),
            &root,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
        assert!(startup.tools.is_none());
        assert_eq!(startup.diagnostics.len(), 1);
        assert!(startup.diagnostics[0].contains("invalid MCP config"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mcp_image_result_is_typed_and_mime_mismatch_omits_the_image_with_a_notice() {
        let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
        let result = convert_tool_result(
            "picture",
            CallToolResult::success(vec![
                ContentBlock::text("a picture"),
                ContentBlock::image(data, "image/png"),
            ]),
            None,
        );
        assert!(!result.is_error, "{}", result.value);
        assert_eq!(result.images.len(), 1);
        assert!(
            result.value["content"]
                .as_str()
                .unwrap()
                .contains("[image: image/png]")
        );
        assert!(result.value.get("raw_result_path").is_none());
        let mismatch = convert_tool_result(
            "picture",
            CallToolResult::success(vec![ContentBlock::image(data, "image/jpeg")]),
            None,
        );
        // Validation still withholds the mismatched image; the server's
        // success survives with an indexed conversion notice instead of a
        // replacement execution failure.
        assert!(!mismatch.is_error, "{}", mismatch.value);
        assert!(mismatch.images.is_empty());
        let notice = mismatch.value["conversion_notices"][0].as_str().unwrap();
        assert!(notice.contains("block 0"), "{notice}");
        assert!(notice.contains("MIME type"), "{notice}");
        assert_eq!(mismatch.value["omitted_blocks"], 1);
        let raw = mismatch.value["raw_result_path"].as_str().unwrap();
        assert!(mismatch.value["raw_result_error"].is_null());
        let saved: Value = serde_json::from_slice(&fs::read(raw).unwrap()).unwrap();
        assert_eq!(saved["content"].as_array().unwrap().len(), 1);
        fs::remove_file(raw).unwrap();
    }

    #[tokio::test]
    async fn large_mcp_result_remains_readable_without_entering_model_context() {
        let full_text = "important start\n".to_owned() + &"é".repeat(40 * 1024) + "\nimportant end";
        let output = convert_tool_result(
            "report",
            CallToolResult::success(vec![ContentBlock::text(full_text.clone())]),
            None,
        );
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["truncated"], true);
        assert!(output.value["content"].as_str().unwrap().len() < 64 * 1024);
        let path = output.value["full_output_path"].as_str().unwrap();
        let saved = read_native_snapshot(path).await;
        assert_eq!(saved["content"], full_text);
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_file(path).unwrap();
    }
}
