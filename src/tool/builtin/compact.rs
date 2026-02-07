//! Built-in compact tool for agent-triggered context compaction.
//!
//! This is a sentinel tool -- the actual compaction is triggered by the agent
//! loop after detecting this tool was called. The tool itself just validates
//! the request and returns a placeholder message.

use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;

/// Tool name constant, used by both the tool definition and the agent loop.
pub const COMPACT_TOOL_NAME: &str = "compact";

pub struct CompactTool;

#[async_trait]
impl Tool for CompactTool {
    fn name(&self) -> &'static str {
        COMPACT_TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Compact the conversation context to free up space. Use when: \
         (1) switching to a new task area, (2) after completing major work, \
         or (3) if you notice context degradation (forgetting earlier details, \
         repeating questions). Returns a summary of what was preserved."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Brief reason for compacting (e.g., 'switching to new feature', 'milestone complete')"
                }
            }
        })
    }

    fn danger_level(&self) -> DangerLevel {
        DangerLevel::Safe
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        // The actual compaction is handled by the agent loop after detecting
        // this tool was called. This placeholder is replaced with the real
        // compaction result (e.g. "Compacted: 147k → 110k tokens").
        Ok(ToolResult {
            content: "Compaction will be performed after this tool call completes.".to_string(),
            is_error: false,
            metadata: None,
        })
    }
}
