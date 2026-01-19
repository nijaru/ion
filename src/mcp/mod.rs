use anyhow::Result;
use async_trait::async_trait;
use mcp_sdk_rs::client::Client;
use mcp_sdk_rs::session::Session;
use mcp_sdk_rs::transport::Message;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc};

use crate::tool::types::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("Failed to spawn server: {0}")]
    SpawnFailed(String),

    #[error("Connection error: {0}")]
    Connection(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Disconnected")]
    Disconnected,

    #[error("Internal error: {0}")]
    Internal(String),
}

pub struct McpClient {
    client: Client,
    _to_session_tx: mpsc::UnboundedSender<Message>,
}

impl McpClient {
    pub async fn spawn(_name: String, config: McpServerConfig) -> Result<Self, McpError> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::inherit());

        if let Some(env) = config.env {
            cmd.envs(env);
        }

        let (to_session_tx, to_session_rx) = mpsc::unbounded_channel::<Message>();
        let (from_session_tx, from_session_rx) = mpsc::unbounded_channel::<Message>();

        let session = Session::Local {
            handler: None,
            command: cmd,
            receiver: Arc::new(Mutex::new(to_session_rx)),
            sender: Arc::new(from_session_tx),
        };

        session
            .start()
            .await
            .map_err(|e| McpError::SpawnFailed(e.to_string()))?;

        let client = Client::new(to_session_tx.clone(), from_session_rx);

        // Initialize MCP
        client
            .request(
                "initialize",
                Some(json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "ion",
                        "version": "0.1.0"
                    }
                })),
            )
            .await
            .map_err(|e| McpError::Protocol(e.to_string()))?;

        client
            .notify("notifications/initialized", None)
            .await
            .map_err(|e| McpError::Protocol(e.to_string()))?;

        Ok(Self {
            client,
            _to_session_tx: to_session_tx,
        })
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
        let response = self
            .client
            .request("tools/list", None)
            .await
            .map_err(|e| McpError::Protocol(e.to_string()))?;

        let tools: Vec<McpToolDef> = serde_json::from_value(response["tools"].clone())
            .map_err(|e| McpError::Protocol(e.to_string()))?;

        Ok(tools)
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolResult, McpError> {
        let response = self
            .client
            .request(
                "tools/call",
                Some(json!({
                    "name": name,
                    "arguments": arguments
                })),
            )
            .await
            .map_err(|e| McpError::Protocol(e.to_string()))?;

        let content = response["content"].clone();
        let is_error = response
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // MCP content is often an array of objects like { type: "text", text: "..." }
        let text_content = if let Some(arr) = content.as_array() {
            arr.iter()
                .filter_map(|item| item.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            content.to_string()
        };

        Ok(ToolResult {
            content: text_content,
            is_error,
            metadata: Some(response),
        })
    }
}

pub struct McpTool {
    pub client: Arc<McpClient>,
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        self.client
            .call_tool(&self.name, args)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("MCP error: {}", e)))
    }

    fn danger_level(&self) -> DangerLevel {
        DangerLevel::Restricted
    }
}

#[derive(Default)]
pub struct McpManager {
    clients: Vec<Arc<McpClient>>,
}

impl McpManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn add_server(
        &mut self,
        name: &str,
        config: McpServerConfig,
    ) -> Result<(), McpError> {
        let client = McpClient::spawn(name.to_string(), config).await?;
        self.clients.push(Arc::new(client));
        Ok(())
    }

    pub async fn get_all_tools(&self) -> Vec<Box<dyn Tool>> {
        let mut all_tools = Vec::new();
        for client in &self.clients {
            match client.list_tools().await {
                Ok(tools) => {
                    for tool_def in tools {
                        all_tools.push(Box::new(McpTool {
                            client: client.clone(),
                            name: tool_def.name,
                            description: tool_def.description,
                            input_schema: tool_def.input_schema,
                        }) as Box<dyn Tool>);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to list tools for an MCP server: {}", e);
                }
            }
        }
        all_tools
    }
}
