//! Explicit MCP tool connections. Session facts remain in ion-core.
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::{
        Arc, RwLock as SyncRwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use futures_util::future::join_all;
use ion_ai::{BoxFuture, ToolSpec};
use ion_core::{
    CodingToolOutput, CodingToolSource, ToolActivityKind, ToolDefinition, ToolExposure,
    ToolPresentation, ToolRegistration, ToolResultSchema,
};
use rmcp::{
    ClientHandler, RoleClient,
    service::{NotificationContext, RunningService},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::fs;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

mod config;
mod connection;
mod conversion;
mod framing;
mod http;
mod invocation;
mod request;
mod resource_tools;
mod send;
mod stderr;
use connection::{Connection, combine_cleanup};
use invocation::McpExecutor;

#[cfg(test)]
mod invocation_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod metadata_tests;
#[cfg(test)]
mod resource_tests;

pub use config::{McpConfig, McpExposure, McpHttpServer, McpListing, McpServer, McpStdioServer};
use config::{validate_name, validate_server};

#[derive(Clone)]
struct ChangeHandler(Arc<AtomicU64>);

impl ClientHandler for ChangeHandler {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

type Client = RunningService<RoleClient, ChangeHandler>;
struct RegisteredTool {
    spec: ToolSpec,
    original_name: String,
    result_schema: Option<ToolResultSchema>,
}

type RegisteredTools = Vec<RegisteredTool>;
type Catalog = BTreeMap<String, RegisteredTools>;

struct Server {
    name: String,
    exposure: McpExposure,
    /// The initialize handshake advertised resource operations. Capabilities are
    /// fixed for the connection, so callers do not need a second discovery round.
    resources: bool,
    client: Arc<RwLock<Option<Connection>>>,
    stderr: Option<Arc<stderr::Log>>,
    catalog: Arc<SyncRwLock<Catalog>>,
    tool_list_version: Arc<AtomicU64>,
    refreshed_version: AtomicU64,
    retiring: CancellationToken,
    retirement: Mutex<Option<Retirement>>,
}

struct Retirement {
    work: tokio::task::JoinHandle<std::result::Result<(), String>>,
    result: Option<std::result::Result<(), String>>,
}

pub struct McpTools {
    servers: Vec<Arc<Server>>,
    catalog: Arc<SyncRwLock<Catalog>>,
    refresh_gate: Mutex<()>,
}

#[derive(Default)]
pub struct McpStartup {
    pub tools: Option<Arc<McpTools>>,
    pub diagnostics: Vec<String>,
}

impl McpTools {
    pub async fn connect(config: &McpConfig, cwd: &Path) -> McpStartup {
        let (saved, mut diagnostics) = match config.load_startup() {
            Ok(saved) => saved,
            Err(error) => {
                return McpStartup {
                    diagnostics: vec![format!("{error:#}")],
                    ..McpStartup::default()
                };
            }
        };
        let catalog = Arc::new(SyncRwLock::new(Catalog::new()));
        let mut servers = Vec::new();
        let attempts = saved
            .into_iter()
            .map(|(name, definition)| Self::connect_server(name, definition, cwd, catalog.clone()));
        for attempt in join_all(attempts).await {
            match attempt {
                Ok((server, discovered)) => {
                    let published = publish_tools(&catalog, &server.name, discovered);
                    match published {
                        Ok(()) => {
                            if let Some(notice) = server
                                .stderr
                                .as_ref()
                                .and_then(|log| log.notice(&server.name))
                            {
                                diagnostics.push(notice);
                            }
                            servers.push(Arc::new(server));
                        }
                        Err(error) => {
                            let closed = combine_cleanup::<()>(Err(error), server.shutdown().await);
                            diagnostics.push(format!("{:#}", closed.unwrap_err()));
                        }
                    }
                }
                Err(error) => diagnostics.push(format!("{error:#}")),
            }
        }
        let tools = (!servers.is_empty()).then(|| {
            Arc::new(Self {
                servers,
                catalog,
                refresh_gate: Mutex::new(()),
            })
        });
        McpStartup { tools, diagnostics }
    }

    async fn connect_server(
        name: String,
        definition: McpServer,
        cwd: &Path,
        catalog: Arc<SyncRwLock<Catalog>>,
    ) -> Result<(Server, RegisteredTools)> {
        validate_name(&name)?;
        validate_server(&definition)?;
        let changed = Arc::new(AtomicU64::new(0));
        let handler = ChangeHandler(changed.clone());
        let exposure = definition.exposure();
        let client = Connection::start(&name, definition, cwd, handler).await?;
        let peer = client.client.peer_info();
        let resources = exposure != McpExposure::Hidden
            && peer
                .as_ref()
                .is_some_and(|info| info.capabilities.resources.is_some());
        let advertises_tools = peer
            .as_ref()
            .is_none_or(|info| info.capabilities.tools.is_some());
        let discovered = if advertises_tools {
            discover_tools(&name, &client.client).await
        } else {
            Ok(Vec::new())
        };
        let stderr = client.stderr_log();
        match discovered {
            Ok(tools) => Ok((
                Server {
                    name,
                    exposure,
                    resources,
                    client: Arc::new(RwLock::new(Some(client))),
                    stderr,
                    catalog,
                    tool_list_version: changed,
                    refreshed_version: AtomicU64::new(0),
                    retiring: CancellationToken::new(),
                    retirement: Mutex::new(None),
                },
                tools,
            )),
            Err(error) => {
                // A listing failure can be explained only on the server's
                // stderr. Join capture before reading it: stdout and stderr
                // have no cross-stream ordering, so the final bounded prefix
                // is the honest reference for this startup diagnostic.
                let shutdown = client.shutdown().await;
                let error = match stderr.as_ref().and_then(|log| log.notice(&name)) {
                    Some(notice) => error.context(notice),
                    None => error,
                };
                combine_cleanup(Err(error), shutdown)
            }
        }
    }

    async fn refresh_changed(&self, stop: CancellationToken) -> Vec<String> {
        let mut diagnostics = Vec::new();
        let _gate = tokio::select! {
            () = stop.cancelled() => return diagnostics,
            guard = self.refresh_gate.lock() => guard,
        };
        for server in &self.servers {
            let version = server.tool_list_version.load(Ordering::Acquire);
            if version == server.refreshed_version.load(Ordering::Acquire) {
                continue;
            }
            let client = tokio::select! {
                guard = server.client.read() => guard,
                () = stop.cancelled() => break,
            };
            let Some(client) = client.as_ref() else {
                diagnostics.push(format!("MCP server {} is closed", server.name));
                server.refreshed_version.store(version, Ordering::Release);
                continue;
            };
            let updated = tokio::select! {
                () = server.retiring.cancelled() => break,
                () = stop.cancelled() => break,
                result = discover_tools(&server.name, &client.client) =>
                    result.and_then(|updated| publish_tools(&self.catalog, &server.name, updated)),
            };
            match updated {
                Ok(()) => {}
                Err(error) => diagnostics.push(format!("{error:#}")),
            }
            // A failed listing keeps the previous snapshot and warns once.
            // A notification that arrived during this listing has a newer
            // version and will still be refreshed at the next boundary.
            server.refreshed_version.store(version, Ordering::Release);
        }
        diagnostics
    }

    /// Retire admission, join services and reap owned root processes.
    /// Outstanding requests need not receive a remote response or caller stop.
    /// Callers must await this on controlled exit, including failed setup.
    pub async fn shutdown(&self) -> Result<()> {
        // Signal every peer before waiting on any connection's read leases.
        for server in &self.servers {
            server.retire();
        }
        let results = join_all(self.servers.iter().map(|server| server.shutdown())).await;
        let errors: Vec<_> = results.into_iter().filter_map(Result::err).collect();
        ensure!(
            errors.is_empty(),
            "{}",
            errors
                .iter()
                .map(|error| format!("{error:#}"))
                .collect::<Vec<_>>()
                .join("; ")
        );
        Ok(())
    }
}

impl Server {
    fn attach_stderr(&self, output: &mut CodingToolOutput) {
        if let Some(snapshot) = self.stderr.as_ref().and_then(|log| log.snapshot()) {
            output.value["server_stderr"][&self.name] = snapshot;
        }
    }

    fn retire(&self) {
        self.retiring.cancel();
    }

    async fn shutdown(&self) -> Result<()> {
        // Direct failure/cancellation paths use the same retirement boundary.
        // A request must release its read lease before joining this close.
        self.retire();
        // Retain a running close task, not a future owned only by its waiter.
        // Cancelling shutdown cannot drop a taken connection or its reader joins.
        let mut retirement = self.retirement.lock().await;
        let retirement = retirement.get_or_insert_with(|| {
            let client = self.client.clone();
            let catalog = self.catalog.clone();
            let name = self.name.clone();
            Retirement {
                work: tokio::spawn(async move {
                    let result = match client.write().await.take() {
                        Some(client) => client.shutdown().await,
                        None => Ok(()),
                    };
                    catalog
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&name);
                    result
                        .with_context(|| format!("MCP server {name} shutdown failed"))
                        .map_err(|error| format!("{error:#}"))
                }),
                result: None,
            }
        });
        if retirement.result.is_none() {
            let result = match (&mut retirement.work).await {
                Ok(result) => result,
                Err(error) => Err(format!(
                    "MCP cleanup task failed; effects may be unknown: {error}"
                )),
            };
            retirement.result = Some(result);
        }
        retirement
            .result
            .as_ref()
            .expect("joined retirement has an outcome")
            .clone()
            .map_err(anyhow::Error::msg)
    }
}

fn publish_tools(
    catalog: &SyncRwLock<Catalog>,
    server: &str,
    tools: RegisteredTools,
) -> Result<()> {
    let mut catalog = catalog
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut names: HashSet<_> = catalog
        .iter()
        .filter(|(name, _)| name.as_str() != server)
        .flat_map(|(_, tools)| tools.iter().map(|tool| tool.spec.name.as_str()))
        .collect();
    for tool in &tools {
        ensure!(
            names.insert(&tool.spec.name),
            "MCP server {server} has colliding tool name {:?} (alias {}) in the complete MCP source",
            tool.original_name,
            tool.spec.name
        );
    }
    catalog.insert(server.to_owned(), tools);
    Ok(())
}

async fn discover_tools(name: &str, client: &Client) -> Result<RegisteredTools> {
    let tools = tokio::time::timeout(Duration::from_secs(10), client.list_all_tools())
        .await
        .with_context(|| format!("MCP server {name} tool listing timed out"))?
        .with_context(|| format!("MCP server {name} tool listing failed"))?;
    let mut registered = Vec::new();
    for tool in tools {
        let tool_name = tool.name.to_string();
        ensure!(
            !tool_name.is_empty(),
            "MCP server {name} listed an empty tool name"
        );
        let exposed = exposed_tool_name(name, &tool_name);
        let description = tool.description.map_or_else(
            || format!("Tool {tool_name} from MCP server {name}"),
            |description| description.into_owned(),
        );
        registered.push(RegisteredTool {
            spec: ToolSpec {
                name: exposed,
                description,
                input_schema: Value::Object(Arc::unwrap_or_clone(tool.input_schema)),
            },
            original_name: tool_name,
            result_schema: tool.output_schema.map(|schema| ToolResultSchema {
                value_path: vec!["structured_content".into()],
                schema: Value::Object(Arc::unwrap_or_clone(schema)),
            }),
        });
    }
    Ok(registered)
}

fn exposed_tool_name(server: &str, original: &str) -> String {
    const MAX_NAME_BYTES: usize = 64;
    const HASH_HEX_BYTES: usize = 16;
    // A length-delimited server component cannot consume a tool's separator.
    let mut exposed = format!("mcp__{}_{server}__", server.len());
    exposed.extend(original.chars().map(|character| {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            character
        } else {
            '_'
        }
    }));
    if exposed.len() <= MAX_NAME_BYTES
        && original
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return exposed;
    }

    let mut hasher = Sha256::new();
    hasher.update(server.as_bytes());
    hasher.update([0]);
    hasher.update(original.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    exposed.truncate(MAX_NAME_BYTES - HASH_HEX_BYTES - 1);
    exposed.push('_');
    exposed.push_str(&hash[..HASH_HEX_BYTES]);
    exposed
}

impl CodingToolSource for McpTools {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        let mut registrations = Vec::new();
        // Snapshot the complete source under one guard, not independently
        // changing server projections. The same guard protects publication.
        let catalog = self
            .catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for server in &self.servers {
            let Some(tools) = catalog.get(&server.name) else {
                continue;
            };
            if server.exposure == McpExposure::Hidden {
                continue;
            }
            for tool in tools {
                registrations.push(ToolRegistration::new(
                    ToolDefinition {
                        spec: tool.spec.clone(),
                        result_schema: tool.result_schema.clone(),
                        presentation: ToolPresentation::static_target(
                            ToolActivityKind::External,
                            tool.original_name.clone(),
                        ),
                        exposure: match server.exposure {
                            McpExposure::Direct => ToolExposure::Direct,
                            McpExposure::Deferred | McpExposure::Hidden => ToolExposure::Deferred,
                        },
                    },
                    Arc::new(McpExecutor {
                        server: server.clone(),
                        original_name: tool.original_name.clone(),
                    }),
                ));
            }
        }
        drop(catalog);
        registrations.extend(resource_tools::registrations(&self.servers));
        registrations
    }

    fn refresh<'a>(&'a self, stop: CancellationToken) -> BoxFuture<'a, Vec<String>> {
        Box::pin(self.refresh_changed(stop))
    }
}

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
                    command: "python3".into(),
                    args: vec!["-c".into(), server.into()],
                    exposure: McpExposure::Direct,
                }),
            )
            .unwrap();
        let startup = McpTools::connect(&config, &root).await;
        assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
        let source = startup.tools.unwrap();
        let tools = ion_core::ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]);
        let issued = tools.snapshot();

        // Another consumer refreshes the source after this request was issued.
        source.servers[0]
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
                    command: "python3".into(), args: vec!["-c".into(), server.into()],
                    exposure: McpExposure::Direct,
                })).unwrap();
                let startup = McpTools::connect(&config, &root).await;
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
                    while source.servers[0].tool_list_version.load(Ordering::Acquire) == 0 {
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
        let startup = McpTools::connect(&McpConfig::new(&root), &root).await;
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
