use anyhow::Result;
use async_trait::async_trait;
use rmcp::model::{CallToolRequestParams, Tool as RmcpTool};
use rmcp::service::{RunningService, ServiceExt};
use rmcp::transport::TokioChildProcess;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use tokio::process::Command;

use crate::tool::types::{ToolError, ToolResult};

/// Trait for MCP tool fallback — allows testing without real MCP servers.
#[async_trait]
pub trait McpFallback: Send + Sync {
    /// Check if a specific tool exists.
    fn has_tool(&self, name: &str) -> bool;
    /// Call a tool by name. Returns None if tool not found.
    async fn call_tool_by_name(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Option<Result<ToolResult, ToolError>>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<std::collections::HashMap<String, String>>,
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

type ClientService = RunningService<rmcp::service::RoleClient, ()>;

pub struct McpClient {
    service: ClientService,
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

        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| McpError::SpawnFailed(e.to_string()))?;

        let service = ().serve(transport)
            .await
            .map_err(|e| McpError::Connection(e.to_string()))?;

        Ok(Self { service })
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
        let result = self
            .service
            .list_tools(Default::default())
            .await
            .map_err(|e| McpError::Protocol(e.to_string()))?;

        Ok(result
            .tools
            .into_iter()
            .map(|t| rmcp_tool_to_def(&t))
            .collect())
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolResult, McpError> {
        let result = self
            .service
            .call_tool(CallToolRequestParams {
                name: name.to_string().into(),
                arguments: arguments.as_object().cloned(),
                meta: None,
                task: None,
            })
            .await
            .map_err(|e| McpError::Protocol(e.to_string()))?;

        let is_error = result.is_error.unwrap_or(false);

        let text_content: String = result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(ToolResult {
            content: text_content,
            is_error,
            metadata: Some(serde_json::to_value(&result).unwrap_or_default()),
        })
    }
}

fn rmcp_tool_to_def(tool: &RmcpTool) -> McpToolDef {
    McpToolDef {
        name: tool.name.to_string(),
        description: tool.description.clone().unwrap_or_default().into_owned(),
        input_schema: serde_json::to_value(&tool.input_schema).unwrap_or_default(),
    }
}

/// Lightweight index entry for an MCP tool.
struct McpToolEntry {
    name: String,
    description: String,
    input_schema: serde_json::Value,
    client: Arc<McpClient>,
}

#[derive(Default)]
pub struct McpManager {
    clients: Vec<Arc<McpClient>>,
    tool_index: Vec<McpToolEntry>,
}

impl McpManager {
    #[must_use]
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

    /// Build the tool index from all connected servers.
    /// Call this after all servers are added.
    pub async fn build_index(&mut self) {
        self.tool_index.clear();
        for client in &self.clients {
            match client.list_tools().await {
                Ok(tools) => {
                    for tool_def in tools {
                        self.tool_index.push(McpToolEntry {
                            name: tool_def.name,
                            description: tool_def.description,
                            input_schema: tool_def.input_schema,
                            client: client.clone(),
                        });
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to list tools for an MCP server: {}", e);
                }
            }
        }
    }

    /// Search tools by keyword (case-insensitive substring match on name + description).
    pub fn search_tools(&self, query: &str) -> Vec<McpToolSearchResult> {
        let query_lower = query.to_lowercase();
        self.tool_index
            .iter()
            .filter(|entry| {
                entry.name.to_lowercase().contains(&query_lower)
                    || entry.description.to_lowercase().contains(&query_lower)
            })
            .map(|entry| McpToolSearchResult {
                name: entry.name.clone(),
                description: entry.description.clone(),
                input_schema: entry.input_schema.clone(),
            })
            .collect()
    }

    /// Check if any tools are indexed.
    #[must_use]
    pub fn has_tools(&self) -> bool {
        !self.tool_index.is_empty()
    }

    /// Get the number of indexed tools.
    #[must_use]
    pub fn tool_count(&self) -> usize {
        self.tool_index.len()
    }
}

#[async_trait]
impl McpFallback for McpManager {
    fn has_tool(&self, name: &str) -> bool {
        self.tool_index.iter().any(|e| e.name == name)
    }

    async fn call_tool_by_name(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Option<Result<ToolResult, ToolError>> {
        let entry = self.tool_index.iter().find(|e| e.name == name)?;
        Some(
            entry
                .client
                .call_tool(name, args)
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("MCP error: {e}"))),
        )
    }
}

/// Result from searching MCP tools.
#[derive(Debug, Clone, Serialize)]
pub struct McpToolSearchResult {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
