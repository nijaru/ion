pub mod builtin;
pub mod permissions;
pub mod types;

pub use permissions::{PermissionMatrix, PermissionStatus};
pub use types::{DangerLevel, DiscoveryCallback, Tool, ToolContext, ToolError, ToolMode, ToolResult};

use crate::hook::{HookContext, HookPoint, HookRegistry, HookResult};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Orchestrates tool discovery and execution with permission checks.
pub struct ToolOrchestrator {
    tools: HashMap<String, Box<dyn Tool>>,
    permissions: RwLock<PermissionMatrix>,
    hooks: RwLock<HookRegistry>,
    mcp_fallback: Option<Arc<crate::mcp::McpManager>>,
}

impl ToolOrchestrator {
    #[must_use]
    pub fn new(mode: ToolMode) -> Self {
        Self {
            tools: HashMap::new(),
            permissions: RwLock::new(PermissionMatrix::new(mode)),
            hooks: RwLock::new(HookRegistry::new()),
            mcp_fallback: None,
        }
    }

    /// Set an MCP manager as fallback for unknown tool names.
    pub fn set_mcp_fallback(&mut self, manager: Arc<crate::mcp::McpManager>) {
        self.mcp_fallback = Some(manager);
    }

    pub fn register_tool(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Register a hook with the orchestrator.
    pub async fn register_hook(&self, hook: Arc<dyn crate::hook::Hook>) {
        self.hooks.write().await.register(hook);
    }

    pub async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        // Sanitize tool name (models sometimes embed args or XML artifacts)
        let name = crate::tui::message_list::sanitize_tool_name(name);

        // Try MCP fallback for unknown tool names
        if !self.tools.contains_key(name)
            && let Some(ref mcp) = self.mcp_fallback
            && let Some(result) = mcp.call_tool_by_name(name, args.clone()).await
        {
            let mcp_result = result?;
            let post_ctx = HookContext::new(HookPoint::PostToolUse)
                .with_tool_name(name)
                .with_tool_output(&mcp_result.content);
            return match self.hooks.read().await.execute(&post_ctx).await {
                HookResult::ReplaceOutput(output) => Ok(ToolResult {
                    content: output,
                    is_error: mcp_result.is_error,
                    metadata: mcp_result.metadata,
                }),
                HookResult::Abort(msg) => {
                    Err(ToolError::ExecutionFailed(format!("Hook aborted: {msg}")))
                }
                _ => Ok(mcp_result),
            };
        }

        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::ExecutionFailed(format!("Tool not found: {name}")))?;

        // Run PreToolUse hooks
        let pre_ctx = HookContext::new(HookPoint::PreToolUse)
            .with_tool_name(name)
            .with_tool_input(args.clone());
        let args = match self.hooks.read().await.execute(&pre_ctx).await {
            HookResult::Continue => args,
            HookResult::Skip => {
                return Ok(ToolResult {
                    content: "Tool execution skipped by hook".to_string(),
                    is_error: false,
                    metadata: None,
                });
            }
            HookResult::ReplaceInput(new_args) => new_args,
            HookResult::ReplaceOutput(output) => {
                return Ok(ToolResult {
                    content: output,
                    is_error: false,
                    metadata: None,
                });
            }
            HookResult::Abort(msg) => {
                return Err(ToolError::ExecutionFailed(format!("Hook aborted: {msg}")));
            }
        };

        // For bash, use per-command permission checking
        let status = if name == "bash" {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            self.permissions.read().await.check_command_permission(command)
        } else {
            self.permissions.read().await.check_permission(tool.as_ref())
        };

        let result = match status {
            PermissionStatus::Allowed => tool.execute(args, ctx).await,
            PermissionStatus::Denied(reason) => Err(ToolError::PermissionDenied(reason)),
        }?;

        // Run PostToolUse hooks
        let post_ctx = HookContext::new(HookPoint::PostToolUse)
            .with_tool_name(name)
            .with_tool_output(&result.content);
        match self.hooks.read().await.execute(&post_ctx).await {
            HookResult::ReplaceOutput(output) => Ok(ToolResult {
                content: output,
                is_error: result.is_error,
                metadata: result.metadata,
            }),
            HookResult::Abort(msg) => Err(ToolError::ExecutionFailed(format!("Hook aborted: {msg}"))),
            // Continue, Skip, and ReplaceInput all pass through unchanged for PostToolUse
            HookResult::Continue | HookResult::Skip | HookResult::ReplaceInput(_) => Ok(result),
        }
    }

    pub fn list_tools(&self) -> Vec<&dyn Tool> {
        self.tools
            .values()
            .map(std::convert::AsRef::as_ref)
            .collect()
    }

    pub async fn set_tool_mode(&self, mode: ToolMode) {
        self.permissions.write().await.set_mode(mode);
    }

    pub async fn tool_mode(&self) -> ToolMode {
        self.permissions.read().await.mode()
    }

    #[must_use]
    pub fn with_builtins(mode: ToolMode) -> Self {
        let mut orch = Self::new(mode);
        orch.register_tool(Box::new(builtin::ReadTool));
        orch.register_tool(Box::new(builtin::WriteTool));
        orch.register_tool(Box::new(builtin::EditTool));
        orch.register_tool(Box::new(builtin::GlobTool));
        orch.register_tool(Box::new(builtin::GrepTool));
        orch.register_tool(Box::new(builtin::ListTool));
        orch.register_tool(Box::new(builtin::BashTool));
        orch.register_tool(Box::new(builtin::WebFetchTool::new()));
        orch.register_tool(Box::new(builtin::WebSearchTool::new()));
        orch.register_tool(Box::new(builtin::CompactTool));
        // Note: DiscoverTool requires semantic search backend (not yet implemented)
        orch
    }

    /// Keep only tools in whitelist. Empty whitelist = keep all.
    pub fn filter_tools(&mut self, whitelist: &[String]) {
        if whitelist.is_empty() {
            return;
        }
        self.tools
            .retain(|name, _| whitelist.iter().any(|w| w == name));
    }

    /// Get the number of registered hooks.
    pub async fn hook_count(&self) -> usize {
        self.hooks.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    struct MockTool {
        name: String,
        danger: DangerLevel,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "mock"
        }
        fn parameters(&self) -> serde_json::Value {
            json!({})
        }
        fn danger_level(&self) -> DangerLevel {
            self.danger
        }
        async fn execute(
            &self,
            _: serde_json::Value,
            _: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult {
                content: "ok".into(),
                is_error: false,
                metadata: None,
            })
        }
    }

    #[test]
    fn test_permission_matrix_read_safe() {
        let matrix = PermissionMatrix::new(ToolMode::Read);
        let tool = MockTool {
            name: "test".into(),
            danger: DangerLevel::Safe,
        };
        assert_eq!(matrix.check_permission(&tool), PermissionStatus::Allowed);
    }

    #[test]
    fn test_permission_matrix_read_restricted() {
        let matrix = PermissionMatrix::new(ToolMode::Read);
        let tool = MockTool {
            name: "test".into(),
            danger: DangerLevel::Restricted,
        };
        match matrix.check_permission(&tool) {
            PermissionStatus::Denied(_) => {}
            _ => panic!("Expected Denied"),
        }
    }

    #[test]
    fn test_permission_matrix_write_safe() {
        let matrix = PermissionMatrix::new(ToolMode::Write);
        let tool = MockTool {
            name: "test".into(),
            danger: DangerLevel::Safe,
        };
        assert_eq!(matrix.check_permission(&tool), PermissionStatus::Allowed);
    }

    #[test]
    fn test_permission_matrix_write_all_allowed() {
        let matrix = PermissionMatrix::new(ToolMode::Write);
        let tool = MockTool {
            name: "test".into(),
            danger: DangerLevel::Restricted,
        };
        assert_eq!(matrix.check_permission(&tool), PermissionStatus::Allowed);
    }
}
