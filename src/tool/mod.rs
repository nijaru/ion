pub mod builtin;
pub mod permissions;
pub mod types;

pub use permissions::{PermissionMatrix, PermissionStatus};
pub use types::{
    DangerLevel, Tool, ToolContext, ToolError, ToolMode, ToolResult,
};

use crate::hook::{HookContext, HookPoint, HookRegistry, HookResult};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Outcome of running PreToolUse hooks.
enum PreHookOutcome {
    /// Continue with (possibly modified) args.
    Continue(serde_json::Value),
    /// Hook short-circuited execution with a result (Skip or ReplaceOutput).
    ShortCircuit(ToolResult),
}

/// Orchestrates tool discovery and execution with permission checks.
pub struct ToolOrchestrator {
    tools: HashMap<String, Box<dyn Tool>>,
    permissions: RwLock<PermissionMatrix>,
    hooks: RwLock<HookRegistry>,
    mcp_fallback: Option<Arc<dyn crate::mcp::McpFallback>>,
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

    /// Set an MCP fallback for unknown tool names.
    pub fn set_mcp_fallback(&mut self, fallback: Arc<dyn crate::mcp::McpFallback>) {
        self.mcp_fallback = Some(fallback);
    }

    /// Check if an MCP fallback is configured.
    pub fn has_mcp_fallback(&self) -> bool {
        self.mcp_fallback.is_some()
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
            && mcp.has_tool(name)
        {
            // MCP tools are all DangerLevel::Restricted — block in Read mode
            let mode = self.permissions.read().await.mode();
            if mode == ToolMode::Read {
                return Err(ToolError::PermissionDenied(
                    "MCP tools are blocked in Read mode".to_string(),
                ));
            }

            let args = match self.run_pre_hooks(name, args).await? {
                PreHookOutcome::Continue(args) => args,
                PreHookOutcome::ShortCircuit(result) => return Ok(result),
            };

            let mcp_result = match mcp.call_tool_by_name(name, args).await {
                Some(result) => result?,
                None => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "MCP tool not found: {name}"
                    )));
                }
            };

            return self.run_post_hooks(name, mcp_result).await;
        }

        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::ExecutionFailed(format!("Tool not found: {name}")))?;

        let args = match self.run_pre_hooks(name, args).await? {
            PreHookOutcome::Continue(args) => args,
            PreHookOutcome::ShortCircuit(result) => return Ok(result),
        };

        // For bash, use per-command permission checking
        let status = if name == "bash" {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            self.permissions
                .read()
                .await
                .check_command_permission(command)
        } else {
            self.permissions
                .read()
                .await
                .check_permission(tool.as_ref())
        };

        let result = match status {
            PermissionStatus::Allowed => tool.execute(args, ctx).await,
            PermissionStatus::Denied(reason) => Err(ToolError::PermissionDenied(reason)),
        }?;

        self.run_post_hooks(name, result).await
    }

    /// Run PreToolUse hooks. Returns modified args on Continue/ReplaceInput,
    /// or a short-circuit ToolResult for Skip/ReplaceOutput.
    async fn run_pre_hooks(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<PreHookOutcome, ToolError> {
        let pre_ctx = HookContext::new(HookPoint::PreToolUse)
            .with_tool_name(name)
            .with_tool_input(args.clone());
        match self.hooks.read().await.execute(&pre_ctx).await {
            HookResult::Continue => Ok(PreHookOutcome::Continue(args)),
            HookResult::ReplaceInput(new_args) => Ok(PreHookOutcome::Continue(new_args)),
            HookResult::Skip => Ok(PreHookOutcome::ShortCircuit(ToolResult {
                content: "Tool execution skipped by hook".to_string(),
                is_error: false,
                metadata: None,
            })),
            HookResult::ReplaceOutput(output) => Ok(PreHookOutcome::ShortCircuit(ToolResult {
                content: output,
                is_error: false,
                metadata: None,
            })),
            HookResult::Abort(msg) => {
                Err(ToolError::ExecutionFailed(format!("Hook aborted: {msg}")))
            }
        }
    }

    /// Run PostToolUse hooks, returning the (possibly modified) result.
    async fn run_post_hooks(
        &self,
        name: &str,
        result: ToolResult,
    ) -> Result<ToolResult, ToolError> {
        let post_ctx = HookContext::new(HookPoint::PostToolUse)
            .with_tool_name(name)
            .with_tool_output(&result.content);
        match self.hooks.read().await.execute(&post_ctx).await {
            HookResult::ReplaceOutput(output) => Ok(ToolResult {
                content: output,
                is_error: result.is_error,
                metadata: result.metadata,
            }),
            HookResult::Abort(msg) => {
                Err(ToolError::ExecutionFailed(format!("Hook aborted: {msg}")))
            }
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

    /// Mock MCP fallback that reports a tool exists but returns a canned result.
    struct MockMcpFallback {
        tools: Vec<String>,
        result: Option<Result<ToolResult, ToolError>>,
    }

    #[async_trait]
    impl crate::mcp::McpFallback for MockMcpFallback {
        fn has_tool(&self, name: &str) -> bool {
            self.tools.iter().any(|t| t == name)
        }
        async fn call_tool_by_name(
            &self,
            _name: &str,
            _args: serde_json::Value,
        ) -> Option<Result<ToolResult, ToolError>> {
            self.result.clone()
        }
    }

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            session_id: "test".into(),
            abort_signal: tokio_util::sync::CancellationToken::new(),
            no_sandbox: false,
            index_callback: None,
        }
    }

    #[tokio::test]
    async fn test_mcp_fallback_blocked_in_read_mode() {
        let mut orch = ToolOrchestrator::new(ToolMode::Read);
        orch.set_mcp_fallback(Arc::new(MockMcpFallback {
            tools: vec!["mcp_dangerous_tool".into()],
            result: None, // should never be reached
        }));

        let result = orch
            .call_tool("mcp_dangerous_tool", json!({}), &test_ctx())
            .await;
        match result {
            Err(ToolError::PermissionDenied(msg)) => {
                assert!(
                    msg.contains("Read mode"),
                    "Expected Read mode message, got: {msg}"
                );
            }
            other => panic!("Expected PermissionDenied, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_mcp_fallback_allowed_in_write_mode() {
        let mut orch = ToolOrchestrator::new(ToolMode::Write);
        orch.set_mcp_fallback(Arc::new(MockMcpFallback {
            tools: vec!["mcp_tool".into()],
            result: Some(Ok(ToolResult {
                content: "mcp result".into(),
                is_error: false,
                metadata: None,
            })),
        }));

        let result = orch
            .call_tool("mcp_tool", json!({}), &test_ctx())
            .await
            .unwrap();
        assert_eq!(result.content, "mcp result");
    }

    #[tokio::test]
    async fn test_mcp_fallback_runs_pre_hooks() {
        let mut orch = ToolOrchestrator::new(ToolMode::Write);
        orch.set_mcp_fallback(Arc::new(MockMcpFallback {
            tools: vec!["mcp_tool".into()],
            result: None, // should never be reached — hook aborts first
        }));

        // Register an abort hook
        use crate::hook::{Hook, HookContext, HookPoint, HookResult as HR};
        struct AbortHook;
        #[async_trait]
        impl Hook for AbortHook {
            fn hook_point(&self) -> HookPoint {
                HookPoint::PreToolUse
            }
            async fn execute(&self, _ctx: &HookContext) -> HR {
                HR::Abort("blocked by hook".into())
            }
        }
        orch.register_hook(Arc::new(AbortHook)).await;

        let result = orch.call_tool("mcp_tool", json!({}), &test_ctx()).await;
        match result {
            Err(ToolError::ExecutionFailed(msg)) => {
                assert!(
                    msg.contains("blocked by hook"),
                    "Expected hook abort, got: {msg}"
                );
            }
            other => panic!("Expected ExecutionFailed from hook abort, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_mcp_fallback_skip_hook() {
        let mut orch = ToolOrchestrator::new(ToolMode::Write);
        orch.set_mcp_fallback(Arc::new(MockMcpFallback {
            tools: vec!["mcp_tool".into()],
            result: None, // should never be reached — hook skips
        }));

        use crate::hook::{Hook, HookContext, HookPoint, HookResult as HR};
        struct SkipHook;
        #[async_trait]
        impl Hook for SkipHook {
            fn hook_point(&self) -> HookPoint {
                HookPoint::PreToolUse
            }
            async fn execute(&self, _ctx: &HookContext) -> HR {
                HR::Skip
            }
        }
        orch.register_hook(Arc::new(SkipHook)).await;

        let result = orch
            .call_tool("mcp_tool", json!({}), &test_ctx())
            .await
            .unwrap();
        assert_eq!(result.content, "Tool execution skipped by hook");
    }
}
