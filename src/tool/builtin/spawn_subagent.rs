//! Tool for spawning subagents to handle tasks.

use crate::agent::subagent::{SubagentRegistry, run_subagent};
use crate::provider::LlmApi;
use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolMode, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Tool for spawning subagents to handle delegated tasks.
pub struct SpawnSubagentTool {
    registry: Arc<SubagentRegistry>,
    provider: Arc<dyn LlmApi>,
    mode: ToolMode,
}

impl SpawnSubagentTool {
    pub fn new(registry: Arc<SubagentRegistry>, provider: Arc<dyn LlmApi>, mode: ToolMode) -> Self {
        Self { registry, provider, mode }
    }
}

#[async_trait]
impl Tool for SpawnSubagentTool {
    fn name(&self) -> &'static str {
        "spawn_subagent"
    }

    fn description(&self) -> &'static str {
        "Spawn a subagent to handle a delegated task. Subagents run with restricted tools and isolated state."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Name of the subagent to spawn (e.g. 'explorer', 'planner')"
                },
                "task": {
                    "type": "string",
                    "description": "The task for the subagent to complete"
                }
            },
            "required": ["name", "task"]
        })
    }

    fn danger_level(&self) -> DangerLevel {
        // Spawning agents has security implications
        DangerLevel::Restricted
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("name is required".to_string()))?;

        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("task is required".to_string()))?;

        // Look up subagent config
        let config = self
            .registry
            .get(name)
            .cloned()
            .ok_or_else(|| ToolError::InvalidArgs(format!("Subagent not found: {name}")))?;

        // Run the subagent (inherits parent's tool mode)
        let result = run_subagent(&config, task, self.provider.clone(), self.mode)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Subagent failed: {e}")))?;

        let status = if result.was_truncated {
            format!("Completed (truncated at {} turns)", result.turns_used)
        } else {
            format!("Completed in {} turns", result.turns_used)
        };

        Ok(ToolResult {
            content: result.output,
            is_error: false,
            metadata: Some(json!({
                "subagent": name,
                "turns_used": result.turns_used,
                "was_truncated": result.was_truncated,
                "status": status
            })),
        })
    }
}
