//! Explicit local MCP tool connections. Session facts remain in ion-core.
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ion_ai::{BoxFuture, ImageMime, MAX_SOURCE_BYTES, ToolCall, ToolSpec, normalize_image};
use ion_core::{CodingToolHost, CodingToolOutput};
use rmcp::{
    RoleClient,
    model::{CallToolRequestParams, CallToolResult, ContentBlock},
    service::{RunningService, ServiceExt},
    transport::TokioChildProcess,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::model_setup::write_json;

#[derive(Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedServers {
    servers: BTreeMap<String, McpServer>,
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

    pub fn list(&self) -> Result<BTreeMap<String, McpServer>> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice::<SavedServers>(&bytes)
                .with_context(|| format!("invalid MCP config {}", self.path.display()))?
                .servers),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error).context("cannot read MCP config"),
        }
    }

    pub fn add(&self, name: &str, server: McpServer) -> Result<()> {
        validate_name(name)?;
        ensure!(!server.command.trim().is_empty(), "MCP command is empty");
        let mut servers = self.list()?;
        ensure!(
            !servers.contains_key(name),
            "MCP server {name} already exists"
        );
        servers.insert(name.to_owned(), server);
        write_json(&self.path, &SavedServers { servers })
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let mut servers = self.list()?;
        ensure!(
            servers.remove(name).is_some(),
            "MCP server {name} was not found"
        );
        write_json(&self.path, &SavedServers { servers })
    }
}

type Client = RunningService<RoleClient, ()>;

struct Server {
    name: String,
    client: RwLock<Option<Client>>,
}

pub struct McpTools {
    servers: Vec<Server>,
    specs: Vec<ToolSpec>,
    routes: HashMap<String, (usize, String)>,
}

impl McpTools {
    pub async fn connect(config: &McpConfig, cwd: &Path) -> Result<Option<Arc<Self>>> {
        let saved = config.list()?;
        if saved.is_empty() {
            return Ok(None);
        }
        let mut servers = Vec::new();
        let mut specs = Vec::new();
        let mut routes = HashMap::new();
        for (name, definition) in saved {
            validate_name(&name)?;
            let mut command = Command::new(&definition.command);
            command.args(&definition.args).current_dir(cwd);
            let transport = TokioChildProcess::new(command)
                .with_context(|| format!("cannot start MCP server {name}"))?;
            let client = tokio::time::timeout(Duration::from_secs(10), ().serve(transport))
                .await
                .with_context(|| format!("MCP server {name} initialization timed out"))?
                .with_context(|| format!("MCP server {name} initialization failed"))?;
            let discovered = tokio::time::timeout(Duration::from_secs(10), client.list_all_tools())
                .await
                .with_context(|| format!("MCP server {name} tool listing timed out"))?
                .with_context(|| format!("MCP server {name} tool listing failed"))?;
            let index = servers.len();
            for tool in discovered {
                let tool_name = tool.name.to_string();
                validate_name(&tool_name)?;
                let exposed = format!("mcp__{name}__{tool_name}");
                ensure!(
                    exposed.len() <= 64,
                    "MCP tool name {exposed} exceeds provider's 64-character bound"
                );
                ensure!(
                    !routes.contains_key(&exposed),
                    "duplicate MCP tool name {exposed}"
                );
                let description = tool.description.map_or_else(
                    || format!("Tool {tool_name} from MCP server {name}"),
                    |description| description.into_owned(),
                );
                specs.push(ToolSpec {
                    name: exposed.clone(),
                    description,
                    input_schema: Value::Object((*tool.input_schema).clone()),
                });
                routes.insert(exposed, (index, tool_name));
            }
            servers.push(Server {
                name,
                client: RwLock::new(Some(client)),
            });
        }
        Ok(Some(Arc::new(Self {
            servers,
            specs,
            routes,
        })))
    }

    /// Close every child process before the client runtime exits.
    pub async fn shutdown(&self) {
        for server in &self.servers {
            if let Some(mut client) = server.client.write().await.take() {
                let _ = client.close_with_timeout(Duration::from_secs(3)).await;
            }
        }
    }
}

impl CodingToolHost for McpTools {
    fn specs(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, CodingToolOutput> {
        Box::pin(async move {
            let Some((index, tool_name)) = self.routes.get(&call.name) else {
                return tool_error(format!("unknown MCP tool: {}", call.name));
            };
            let server = &self.servers[*index];
            let client = server.client.read().await;
            let Some(client) = client.as_ref() else {
                return tool_error(format!("MCP server {} is closed", server.name));
            };
            let Some(args) = call.arguments.as_object() else {
                return tool_error("MCP tool arguments must be an object".to_owned());
            };
            let request =
                CallToolRequestParams::new(tool_name.clone()).with_arguments(args.clone());
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
    let value =
        json!({"content":content.join("\n"),"structured_content":result.structured_content});
    if !serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() <= 64 * 1024) {
        return tool_error(format!("MCP tool {name} result exceeds 64 KiB text bound"));
    }
    CodingToolOutput {
        value,
        images,
        is_error: result.is_error.unwrap_or(false),
    }
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
}
