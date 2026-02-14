//! Tool for searching available MCP tools by keyword.

use crate::mcp::McpManager;
use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Search available MCP tools by keyword.
pub struct McpToolsTool {
    manager: Arc<McpManager>,
}

impl McpToolsTool {
    pub fn new(manager: Arc<McpManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for McpToolsTool {
    fn name(&self) -> &str {
        "mcp_tools"
    }

    fn description(&self) -> &str {
        "Search available MCP tools by keyword. Returns matching tool names, descriptions, and input schemas. Use this to discover what MCP tools are available before calling them."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query to match against tool names and descriptions"
                }
            },
            "required": ["query"]
        })
    }

    fn danger_level(&self) -> DangerLevel {
        DangerLevel::Safe
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("query is required".to_string()))?;

        let results = self.manager.search_tools(query);

        if results.is_empty() {
            return Ok(ToolResult {
                content: format!("No MCP tools found matching '{query}'."),
                is_error: false,
                metadata: None,
            });
        }

        let mut output = format!(
            "Found {} MCP tool(s) matching '{query}':\n\n",
            results.len()
        );
        for tool in &results {
            output.push_str(&format!("## {}\n{}\n\n", tool.name, tool.description));
            output.push_str(&format!(
                "Schema: {}\n\n",
                serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default()
            ));
        }

        Ok(ToolResult {
            content: output,
            is_error: false,
            metadata: Some(json!({
                "count": results.len(),
                "tools": results.iter().map(|t| &t.name).collect::<Vec<_>>(),
            })),
        })
    }
}
