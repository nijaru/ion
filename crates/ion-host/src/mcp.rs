//! Explicit MCP tool connections. Session facts remain in ion-core.
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock as SyncRwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::future::join_all;
use ion_ai::{BoxFuture, ImageMime, MAX_SOURCE_BYTES, ToolCall, ToolSpec, normalize_image};
use ion_core::{CodingToolHost, CodingToolOutput};
use rmcp::{
    ClientHandler, RoleClient,
    model::{CallToolRequestParams, CallToolResult, ContentBlock},
    service::{NotificationContext, RunningService, ServiceExt},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::model_setup::write_json;

#[derive(Clone, Serialize)]
#[serde(untagged)]
pub enum McpServer {
    Stdio(McpStdioServer),
    Http(McpHttpServer),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpStdioServer {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpHttpServer {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token_env: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServers {
    servers: BTreeMap<String, Value>,
}

pub struct McpListing {
    pub servers: BTreeMap<String, McpServer>,
    pub diagnostics: Vec<String>,
}

pub struct McpConfig {
    path: PathBuf,
}

impl McpConfig {
    pub fn new(config_root: &Path) -> Self {
        Self {
            path: config_root.join("mcp.json"),
        }
    }

    pub fn list(&self) -> Result<McpListing> {
        let mut servers = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for (name, value) in self.read_raw()? {
            match parse_server(&name, value) {
                Ok(server) => {
                    servers.insert(name, server);
                }
                Err(error) => diagnostics.push(format!("{error:#}")),
            }
        }
        Ok(McpListing {
            servers,
            diagnostics,
        })
    }

    fn load_startup(&self) -> Result<(BTreeMap<String, McpServer>, Vec<String>)> {
        let listing = self.list()?;
        Ok((listing.servers, listing.diagnostics))
    }

    fn read_raw(&self) -> Result<BTreeMap<String, Value>> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice::<RawServers>(&bytes)
                .with_context(|| format!("invalid MCP config {}", self.path.display()))?
                .servers),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error).context("cannot read MCP config"),
        }
    }

    pub fn add(&self, name: &str, server: McpServer) -> Result<()> {
        validate_name(name)?;
        validate_server(&server)?;
        let mut servers = self.read_raw()?;
        ensure!(
            !servers.contains_key(name),
            "MCP server {name} already exists"
        );
        servers.insert(name.to_owned(), serde_json::to_value(server)?);
        write_json(&self.path, &RawServers { servers })
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let mut servers = self.read_raw()?;
        ensure!(
            servers.remove(name).is_some(),
            "MCP server {name} was not found"
        );
        write_json(&self.path, &RawServers { servers })
    }
}

fn parse_server(name: &str, value: Value) -> Result<McpServer> {
    (|| -> Result<McpServer> {
        validate_name(name)?;
        let object = value.as_object().context("MCP server must be an object")?;
        let server = match (object.contains_key("command"), object.contains_key("url")) {
            (true, false) => McpServer::Stdio(serde_json::from_value(value)?),
            (false, true) => McpServer::Http(serde_json::from_value(value)?),
            (true, true) => bail!("MCP server cannot combine command and URL"),
            (false, false) => bail!("MCP server requires command or URL"),
        };
        validate_server(&server)?;
        Ok(server)
    })()
    .with_context(|| format!("invalid MCP server {name:?}"))
}

fn validate_server(server: &McpServer) -> Result<()> {
    match server {
        McpServer::Stdio(server) => {
            ensure!(!server.command.trim().is_empty(), "MCP command is empty")
        }
        McpServer::Http(server) => {
            let url = Url::parse(&server.url).context("invalid MCP server URL")?;
            ensure!(
                matches!(url.scheme(), "http" | "https"),
                "MCP server URL must use HTTP or HTTPS"
            );
            ensure!(
                url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
                "MCP server URL cannot contain userinfo or fragment"
            );
            if let Some(name) = &server.bearer_token_env {
                ensure!(
                    valid_env_name(name),
                    "invalid MCP bearer token environment variable name"
                );
            }
        }
    }
    Ok(())
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[derive(Clone)]
struct ChangeHandler(Arc<AtomicU64>);

impl ClientHandler for ChangeHandler {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

type Client = RunningService<RoleClient, ChangeHandler>;
type RegisteredTools = Vec<(ToolSpec, String)>;

struct Server {
    name: String,
    client: RwLock<Option<Client>>,
    tool_list_version: Arc<AtomicU64>,
    refreshed_version: AtomicU64,
    refresh_gate: Mutex<()>,
}

pub struct McpTools {
    servers: Vec<Server>,
    discovered: SyncRwLock<Vec<RegisteredTools>>,
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
        let mut servers = Vec::new();
        let mut discovered = Vec::new();
        let attempts = saved
            .into_iter()
            .map(|(name, definition)| Self::connect_server(name, definition, cwd));
        for attempt in join_all(attempts).await {
            match attempt {
                Ok((server, tools)) => {
                    servers.push(server);
                    discovered.push(tools);
                }
                Err(error) => diagnostics.push(format!("{error:#}")),
            }
        }
        let tools = (!servers.is_empty()).then(|| {
            Arc::new(Self {
                servers,
                discovered: SyncRwLock::new(discovered),
            })
        });
        McpStartup { tools, diagnostics }
    }

    async fn connect_server(
        name: String,
        definition: McpServer,
        cwd: &Path,
    ) -> Result<(Server, RegisteredTools)> {
        validate_name(&name)?;
        validate_server(&definition)?;
        let changed = Arc::new(AtomicU64::new(0));
        let handler = ChangeHandler(changed.clone());
        let mut client = match definition {
            McpServer::Stdio(server) => {
                let mut command = Command::new(&server.command);
                command.args(&server.args).current_dir(cwd);
                let transport = TokioChildProcess::new(command)
                    .with_context(|| format!("cannot start MCP server {name}"))?;
                tokio::time::timeout(Duration::from_secs(10), handler.serve(transport))
                    .await
                    .with_context(|| format!("MCP server {name} initialization timed out"))?
                    .with_context(|| format!("MCP server {name} initialization failed"))?
            }
            McpServer::Http(server) => {
                let mut transport = StreamableHttpClientTransportConfig::with_uri(server.url);
                // A 404 can mean a lost MCP session. The SDK default retries
                // ordinary POSTs after reinitialization, but a tool may have
                // executed before the server returned that response.
                transport.reinit_on_expired_session = false;
                if let Some(env_name) = server.bearer_token_env {
                    let token = std::env::var(&env_name)
                        .with_context(|| format!("MCP server {name} needs {env_name}"))?;
                    ensure!(!token.is_empty(), "MCP server {name} needs {env_name}");
                    transport.auth_header = Some(token);
                }
                let transport = StreamableHttpClientTransport::from_config(transport);
                tokio::time::timeout(Duration::from_secs(10), handler.serve(transport))
                    .await
                    .with_context(|| format!("MCP server {name} initialization timed out"))?
                    .with_context(|| format!("MCP server {name} initialization failed"))?
            }
        };
        let discovered = discover_tools(&name, &client).await;
        match discovered {
            Ok(discovered) => Ok((
                Server {
                    name,
                    client: RwLock::new(Some(client)),
                    tool_list_version: changed,
                    refreshed_version: AtomicU64::new(0),
                    refresh_gate: Mutex::new(()),
                },
                discovered,
            )),
            Err(error) => {
                let _ = client.close_with_timeout(Duration::from_secs(3)).await;
                Err(error)
            }
        }
    }

    async fn refresh_changed(&self, stop: CancellationToken) -> Vec<String> {
        let mut diagnostics = Vec::new();
        for (index, server) in self.servers.iter().enumerate() {
            if server.tool_list_version.load(Ordering::Acquire)
                == server.refreshed_version.load(Ordering::Acquire)
            {
                continue;
            }
            let _gate = tokio::select! {
                guard = server.refresh_gate.lock() => guard,
                () = stop.cancelled() => break,
            };
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
                () = stop.cancelled() => break,
                result = discover_tools(&server.name, client) => result,
            };
            match updated {
                Ok(updated) => {
                    self.discovered
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)[index] = updated;
                }
                Err(error) => diagnostics.push(format!("{error:#}")),
            }
            // A failed listing keeps the previous snapshot and warns once.
            // A notification that arrived during this listing has a newer
            // version and will still be refreshed at the next boundary.
            server.refreshed_version.store(version, Ordering::Release);
        }
        diagnostics
    }

    /// Close every child process before the client runtime exits.
    pub async fn shutdown(&self) {
        for server in &self.servers {
            if let Some(mut client) = server.client.write().await.take() {
                let _ = client.close_with_timeout(Duration::from_secs(3)).await;
            }
        }
        for tools in self
            .discovered
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter_mut()
        {
            tools.clear();
        }
    }
}

async fn discover_tools(name: &str, client: &Client) -> Result<RegisteredTools> {
    let tools = tokio::time::timeout(Duration::from_secs(10), client.list_all_tools())
        .await
        .with_context(|| format!("MCP server {name} tool listing timed out"))?
        .with_context(|| format!("MCP server {name} tool listing failed"))?;
    let mut registered = Vec::new();
    let mut exposed_names = HashSet::new();
    for tool in tools {
        let tool_name = tool.name.to_string();
        ensure!(
            !tool_name.is_empty(),
            "MCP server {name} listed an empty tool name"
        );
        let exposed = exposed_tool_name(name, &tool_name);
        ensure!(
            exposed_names.insert(exposed.clone()),
            "MCP server {name} has duplicate or colliding tool name {tool_name:?} (alias {exposed})"
        );
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
    let mut exposed = format!("mcp__{server}__");
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

impl CodingToolHost for McpTools {
    fn specs(&self) -> Vec<ToolSpec> {
        self.discovered
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .flat_map(|tools| tools.iter().map(|(spec, _)| spec.clone()))
            .collect()
    }

    fn refresh_specs<'a>(&'a self, stop: CancellationToken) -> BoxFuture<'a, Vec<String>> {
        Box::pin(self.refresh_changed(stop))
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, CodingToolOutput> {
        Box::pin(async move {
            let route = self
                .discovered
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .enumerate()
                .find_map(|(index, tools)| {
                    tools
                        .iter()
                        .find(|(spec, _)| spec.name == call.name)
                        .map(|(_, name)| (index, name.clone()))
                });
            let Some((index, tool_name)) = route else {
                return tool_error(format!("unknown MCP tool: {}", call.name));
            };
            let server = &self.servers[index];
            let client = server.client.read().await;
            let Some(client) = client.as_ref() else {
                return tool_error(format!("MCP server {} is closed", server.name));
            };
            let Some(args) = call.arguments.as_object() else {
                return tool_error("MCP tool arguments must be an object".to_owned());
            };
            let request = CallToolRequestParams::new(tool_name).with_arguments(args.clone());
            let result = tokio::select! {
                () = stop.cancelled() => return tool_error(format!("MCP tool {} cancelled; effects may be unknown",call.name)),
                result = client.call_tool(request) => result,
            };
            match result {
                Ok(result) => convert_tool_result(&call.name, result),
                Err(error) => tool_error(format!(
                    "MCP server {} tool call failed: {error}",
                    server.name
                )),
            }
        })
    }
}

fn convert_tool_result(name: &str, result: CallToolResult) -> CodingToolOutput {
    let mut content = Vec::new();
    let mut images = Vec::new();
    for part in result.content {
        match part {
            ContentBlock::Text(text) => content.push(text.text),
            ContentBlock::Image(image) => {
                if image.data.len() > MAX_SOURCE_BYTES.div_ceil(3) * 4 {
                    return tool_error(format!(
                        "MCP tool {name} image exceeds 32 MiB source bound"
                    ));
                }
                let bytes = match STANDARD.decode(&image.data) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return tool_error(format!(
                            "MCP tool {name} returned invalid base64 image data"
                        ));
                    }
                };
                let Some(mime) = ImageMime::detect(&bytes) else {
                    return tool_error(format!(
                        "MCP tool {name} returned an unsupported image format"
                    ));
                };
                if mime.as_str() != image.mime_type {
                    return tool_error(format!(
                        "MCP tool {name} image MIME type does not match its data"
                    ));
                }
                let loaded = match normalize_image(&bytes) {
                    Ok(image) => image,
                    Err(error) => {
                        return tool_error(format!(
                            "MCP tool {name} returned an invalid image: {error}"
                        ));
                    }
                };
                content.push(format!("[image: {}]", loaded.content.mime_type().as_str()));
                if let Some(note) = loaded.note {
                    content.push(note);
                }
                images.push(loaded.content);
            }
            _ => {
                return tool_error(format!(
                    "MCP tool {name} returned unsupported non-image media or resource content"
                ));
            }
        }
    }
    let is_error = result.is_error.unwrap_or(false);
    let value =
        json!({"content":content.join("\n"),"structured_content":result.structured_content});
    let encoded = match serde_json::to_string(&value) {
        Ok(encoded) => encoded,
        Err(error) => {
            return tool_error(format!("MCP tool {name} result cannot be encoded: {error}"));
        }
    };
    let value = if encoded.len() <= 64 * 1024 {
        value
    } else {
        let (full_output_path, full_output_error) = match save_full_mcp_output(encoded.as_bytes()) {
            Ok(path) => (Some(path), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let prefix_end = floor_char_boundary(&encoded, 10 * 1024);
        let suffix_start = floor_char_boundary(&encoded, encoded.len() - 10 * 1024);
        json!({
            "content": format!(
                "MCP result truncated from {} bytes. Read full_output_path with read or exec when available.\n{}\n[... omitted middle ...]\n{}",
                encoded.len(), &encoded[..prefix_end], &encoded[suffix_start..]
            ),
            "truncated": true,
            "full_output_path": full_output_path,
            "full_output_error": full_output_error,
        })
    };
    CodingToolOutput {
        value,
        images,
        is_error,
    }
}

fn floor_char_boundary(text: &str, mut offset: usize) -> usize {
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn save_full_mcp_output(bytes: &[u8]) -> Result<PathBuf> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow::anyhow!("cannot name MCP output: {error}"))?;
    let path = std::env::temp_dir().join(format!(
        "ion-mcp-output-{}-{:032x}.json",
        std::process::id(),
        u128::from_ne_bytes(nonce)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    if let Err(error) = file.write_all(bytes) {
        let _ = fs::remove_file(path);
        return Err(error.into());
    }
    Ok(path)
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        bail!("MCP name {name:?} must use only ASCII letters, numbers, '_' or '-'");
    }
    Ok(())
}

fn tool_error(message: String) -> CodingToolOutput {
    CodingToolOutput {
        value: json!({"error":message}),
        images: Vec::new(),
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn model_facing_mcp_names_preserve_distinct_originals() {
        let plain = exposed_tool_name("demo", "admin_tools_list");
        let dotted = exposed_tool_name("demo", "admin.tools.list");
        let long = exposed_tool_name("demo", &"query".repeat(30));
        let other_server = exposed_tool_name("other", "admin.tools.list");
        assert_eq!(plain, "mcp__demo__admin_tools_list");
        assert!(dotted.starts_with("mcp__demo__admin_tools_list_"));
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

    #[test]
    fn remote_config_requires_http_url_and_named_ambient_token() {
        let root = std::env::temp_dir().join(format!("ion-mcp-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let config = McpConfig::new(&root);
        let remote = |url: &str, bearer_token_env: Option<&str>| {
            McpServer::Http(McpHttpServer {
                url: url.into(),
                bearer_token_env: bearer_token_env.map(str::to_owned),
            })
        };
        assert!(
            config
                .add("bad-scheme", remote("file:///tmp/mcp", None))
                .is_err()
        );
        assert!(
            config
                .add("bad-env", remote("https://example.com/mcp", Some("1KEY")))
                .is_err()
        );
        config
            .add("remote", remote("https://example.com/mcp", Some("MCP_KEY")))
            .unwrap();
        assert!(
            matches!(&config.list().unwrap().servers["remote"], McpServer::Http(server) if server.bearer_token_env.as_deref() == Some("MCP_KEY"))
        );
        assert!(
            fs::read_to_string(&config.path)
                .unwrap()
                .contains("MCP_KEY")
        );
        config.remove("remote").unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_server_entry_does_not_hide_valid_startup_servers_or_get_discarded() {
        let root = std::env::temp_dir().join(format!("ion-mcp-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let config = McpConfig::new(&root);
        fs::write(
            &config.path,
            br#"{"servers":{"good":{"command":"echo","args":["ready"]},"bad":{"command":7}}}"#,
        )
        .unwrap();

        let (servers, diagnostics) = config.load_startup().unwrap();
        assert_eq!(servers.len(), 1);
        assert!(matches!(&servers["good"], McpServer::Stdio(server) if server.command == "echo"));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("invalid MCP server \"bad\""));
        assert!(diagnostics[0].contains("expected a string"));
        let listing = config.list().unwrap();
        assert_eq!(listing.servers.len(), 1);
        assert!(listing.servers.contains_key("good"));
        assert_eq!(listing.diagnostics.len(), 1);

        config.remove("good").unwrap();
        config
            .add(
                "new",
                McpServer::Stdio(McpStdioServer {
                    command: "echo".into(),
                    args: Vec::new(),
                }),
            )
            .unwrap();
        let saved = fs::read_to_string(&config.path).unwrap();
        assert!(saved.contains("\"bad\""));
        assert!(saved.contains("\"new\""));
        assert!(!saved.contains("\"good\""));

        // Invalid entries are still addressable by name, so the user can
        // repair the file without hand-editing unrelated JSON.
        config.remove("bad").unwrap();
        let listing = config.list().unwrap();
        assert_eq!(listing.servers.len(), 1);
        assert!(listing.servers.contains_key("new"));
        assert!(listing.diagnostics.is_empty());
        fs::remove_dir_all(root).unwrap();
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

    #[test]
    fn large_mcp_result_remains_readable_without_entering_model_context() {
        let full_text = "important start\n".to_owned() + &"x".repeat(70 * 1024) + "\nimportant end";
        let output = convert_tool_result(
            "report",
            CallToolResult::success(vec![ContentBlock::text(full_text.clone())]),
        );
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["truncated"], true);
        assert!(output.value["content"].as_str().unwrap().len() < 64 * 1024);
        let path = output.value["full_output_path"].as_str().unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["content"], full_text);
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_file(path).unwrap();
    }
}
