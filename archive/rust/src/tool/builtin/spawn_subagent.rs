//! Tool for spawning subagents to handle tasks.

use crate::agent::subagent::{SubagentRegistry, run_subagent};
use crate::provider::LlmApi;
use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolMode, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Shared atomic for live ToolMode (0 = Read, 1 = Write).
pub type SharedToolMode = Arc<AtomicU8>;

/// Create a new shared tool mode from an initial value.
pub fn shared_tool_mode(mode: ToolMode) -> SharedToolMode {
    Arc::new(AtomicU8::new(mode_to_u8(mode)))
}

fn mode_to_u8(mode: ToolMode) -> u8 {
    match mode {
        ToolMode::Read => 0,
        ToolMode::Write => 1,
    }
}

fn u8_to_mode(v: u8) -> ToolMode {
    if v == 0 {
        ToolMode::Read
    } else {
        ToolMode::Write
    }
}

/// Update a shared tool mode atomically.
pub fn set_shared_mode(shared: &SharedToolMode, mode: ToolMode) {
    shared.store(mode_to_u8(mode), Ordering::Relaxed);
}

/// Tool for spawning subagents to handle delegated tasks.
pub struct SpawnSubagentTool {
    registry: Arc<SubagentRegistry>,
    provider: Arc<dyn LlmApi>,
    mode: SharedToolMode,
}

impl SpawnSubagentTool {
    pub fn new(
        registry: Arc<SubagentRegistry>,
        provider: Arc<dyn LlmApi>,
        mode: SharedToolMode,
    ) -> Self {
        Self {
            registry,
            provider,
            mode,
        }
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

        // Read live mode (reflects runtime toggle via Shift+Tab)
        let current_mode = u8_to_mode(self.mode.load(Ordering::Relaxed));

        // Run the subagent (inherits parent's current tool mode)
        let result = run_subagent(&config, task, self.provider.clone(), current_mode)
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
