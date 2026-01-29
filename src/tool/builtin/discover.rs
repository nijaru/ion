use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write as _;

pub struct DiscoverTool;

#[async_trait]
impl Tool for DiscoverTool {
    fn name(&self) -> &'static str {
        "discover"
    }

    fn description(&self) -> &'static str {
        "Search for relevant files, functions, or classes in the codebase using semantic search. Useful when you don't know the exact file path or symbol name."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query (e.g., 'how is authentication handled?', 'where is the TUI loop?')"
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
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("query is required".to_string()))?;

        if let Some(callback) = &ctx.discovery_callback {
            let results = callback(query.to_string())
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("Discovery failed: {e}")))?;

            if results.is_empty() {
                return Ok(ToolResult {
                    content: "No relevant results found in the semantic index. Try using grep or glob for broader search.".to_string(),
                    is_error: false,
                    metadata: None,
                });
            }

            let mut output = String::from("Semantic Search Results:\n");
            for (text, score) in results {
                let _ = writeln!(output, "- {text} (Relevance: {score:.2})");
            }

            Ok(ToolResult {
                content: output,
                is_error: false,
                metadata: None,
            })
        } else {
            Err(ToolError::ExecutionFailed(
                "Discovery callback not available".to_string(),
            ))
        }
    }
}
