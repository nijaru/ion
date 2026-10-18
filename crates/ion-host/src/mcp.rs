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
    ToolPresentation, ToolRegistration,
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
mod http;
mod invocation;
mod resource_tools;
mod send;
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
type RegisteredTools = Vec<(ToolSpec, String)>;
type Catalog = BTreeMap<String, RegisteredTools>;

struct Server {
    name: String,
    exposure: McpExposure,
    /// The initialize handshake advertised resource operations. Capabilities are
    /// fixed for the connection, so callers do not need a second discovery round.
    resources: bool,
    client: RwLock<Option<Connection>>,
    catalog: Arc<SyncRwLock<Catalog>>,
    tool_list_version: Arc<AtomicU64>,
    refreshed_version: AtomicU64,
    retiring: CancellationToken,
    retirement: Mutex<Option<std::result::Result<(), String>>>,
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
                        Ok(()) => servers.push(Arc::new(server)),
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
        match discovered {
            Ok(tools) => Ok((
                Server {
                    name,
                    exposure,
                    resources,
                    client: RwLock::new(Some(client)),
                    catalog,
                    tool_list_version: changed,
                    refreshed_version: AtomicU64::new(0),
                    retiring: CancellationToken::new(),
                    retirement: Mutex::new(None),
                },
                tools,
            )),
            Err(error) => combine_cleanup(Err(error), client.shutdown().await),
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

    /// Join services and reap owned root processes before the runtime exits.
    /// Callers must await this on controlled exit, including failed setup.
    pub async fn shutdown(&self) -> Result<()> {
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
    async fn retire(&self) -> Result<()> {
        self.retiring.cancel();
        self.shutdown().await
    }

    async fn shutdown(&self) -> Result<()> {
        // Concurrent callers join the same retirement, including a tool whose
        // protocol cancellation could not flush. Taking the client alone is
        // not evidence that another caller has finished closing it.
        let mut retirement = self.retirement.lock().await;
        if let Some(result) = retirement.as_ref() {
            return result.clone().map_err(anyhow::Error::msg);
        }
        let result = match self.client.write().await.take() {
            Some(client) => client.shutdown().await,
            None => Ok(()),
        };
        self.catalog
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.name);
        let result = result
            .with_context(|| format!("MCP server {} shutdown failed", self.name))
            .map_err(|error| format!("{error:#}"));
        *retirement = Some(result.clone());
        result.map_err(anyhow::Error::msg)
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
        .flat_map(|(_, tools)| tools.iter().map(|(spec, _)| spec.name.as_str()))
        .collect();
    for (spec, original) in &tools {
        ensure!(
            names.insert(&spec.name),
            "MCP server {server} has colliding tool name {original:?} (alias {}) in the complete MCP source",
            spec.name
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
        registered.push((
            ToolSpec {
                name: exposed,
                description,
                input_schema: Value::Object((*tool.input_schema).clone()),
            },
            tool_name,
        ));
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
            for (spec, original) in tools {
                registrations.push(ToolRegistration::new(
                    ToolDefinition {
                        spec: spec.clone(),
                        presentation: ToolPresentation::static_target(
                            ToolActivityKind::External,
                            original.clone(),
                        ),
                        exposure: match server.exposure {
                            McpExposure::Direct => ToolExposure::Direct,
                            McpExposure::Deferred | McpExposure::Hidden => ToolExposure::Deferred,
                        },
                    },
                    Arc::new(McpExecutor {
                        server: server.clone(),
                        original_name: original.clone(),
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
    use invocation::convert_tool_result;
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
        result = {"tools": [{"name": "probe", "inputSchema": {"type": "object"}}]
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
        assert!(!observed.is_error, "{}", observed.value);
        assert_eq!(observed.value["structured_content"]["called"], "probe");
        assert!(closed.is_error);
        assert_eq!(closed.value["error"], "MCP server bound is closed");
    }

    #[test]
    fn observed_result_waits_for_worker_through_cancellation_and_connection_shutdown() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
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
                  "capabilities": {"tools": {}}, "serverInfo": {"name": "settlement", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "probe", "inputSchema": {"type": "object"}}]}
    elif method == "tools/call":
        with Path("effects").open("a") as effects:
            effects.write("ONCE\n")
        result = {"content": [{"type": "text", "text": "x" * 70000}],
                  "structuredContent": {"observed": 7}, "isError": True}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
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
                    id: "observed".into(), name: "mcp__10_settlement__probe".into(),
                    arguments: json!({}), raw_arguments: None,
                }, task_stop).await
            });
            // An actual server effect precedes its response. Shutdown waits for
            // network consumption, but must not wait for local result work.
            let effect = tokio::time::timeout(Duration::from_secs(2), async {
                while !root.join("effects").exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await;
            let closed = tokio::time::timeout(Duration::from_secs(4), source.shutdown()).await;
            let early = tokio::time::timeout(Duration::from_millis(10), &mut work).await;
            let pending = early.is_err();
            stop.cancel();
            let still_pending = !work.is_finished();
            drop(release);
            blocker.await.unwrap();
            let output = match early {
                Ok(output) => output.unwrap(),
                Err(_) => work.await.unwrap(),
            };
            let effects = fs::read_to_string(root.join("effects")).unwrap();
            fs::remove_dir_all(root).unwrap();
            let path = output.value["full_output_path"].as_str().unwrap();
            let saved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            fs::remove_file(path).unwrap();
            assert!(effect.is_ok(), "server effect not observed");
            closed.expect("conversion held the MCP connection").unwrap();
            assert!(pending && still_pending, "MCP result bypassed owned blocking dispatch");
            assert_eq!(effects, "ONCE\n");
            assert!(output.is_error, "original server error must survive cancellation");
            assert_eq!(saved["content"], "x".repeat(70000));
            assert_eq!(saved["structured_content"]["observed"], 7);
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
    fn mcp_image_result_is_typed_and_mime_mismatch_is_a_visible_error() {
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
        let mismatch = convert_tool_result(
            "picture",
            CallToolResult::success(vec![ContentBlock::image(data, "image/jpeg")]),
        );
        assert!(mismatch.is_error);
        assert!(mismatch.images.is_empty());
        assert!(
            mismatch.value["error"]
                .as_str()
                .unwrap()
                .contains("MIME type")
        );
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
