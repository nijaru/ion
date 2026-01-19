pub mod builtin;
pub mod permissions;
pub mod types;

pub use permissions::{PermissionMatrix, PermissionStatus};
pub use types::*;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Orchestrates tool discovery and execution with permission checks.
pub struct ToolOrchestrator {
    tools: HashMap<String, Box<dyn Tool>>,
    permissions: RwLock<PermissionMatrix>,
    approval_handler: Option<Arc<dyn ApprovalHandler>>,
}

impl ToolOrchestrator {
    pub fn new(mode: ToolMode) -> Self {
        Self {
            tools: HashMap::new(),
            permissions: RwLock::new(PermissionMatrix::new(mode)),
            approval_handler: None,
        }
    }

    pub fn set_approval_handler(&mut self, handler: Arc<dyn ApprovalHandler>) {
        self.approval_handler = Some(handler);
    }

    pub fn register_tool(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolError::ExecutionFailed(format!("Tool not found: {}", name)))?;

        // For bash, use per-command permission checking
        let (status, bash_command) = if name == "bash" {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let perms = self.permissions.read().await;
            (
                perms.check_command_permission(command),
                Some(command.to_string()),
            )
        } else {
            let perms = self.permissions.read().await;
            (perms.check_permission(tool.as_ref()), None)
        };

        match status {
            PermissionStatus::Allowed => tool.execute(args, ctx).await,
            PermissionStatus::NeedsApproval => {
                if let Some(handler) = &self.approval_handler {
                    let response = handler.ask_approval(name, &args).await;
                    match response {
                        ApprovalResponse::Yes => tool.execute(args, ctx).await,
                        ApprovalResponse::No => Err(ToolError::PermissionDenied(
                            "User rejected tool execution".to_string(),
                        )),
                        ApprovalResponse::AlwaysSession => {
                            {
                                let mut perms = self.permissions.write().await;
                                if let Some(ref cmd) = bash_command {
                                    // For bash, allow the specific command
                                    perms.allow_command_session(cmd);
                                } else {
                                    perms.allow_session(name);
                                }
                            }
                            tool.execute(args, ctx).await
                        }
                        ApprovalResponse::AlwaysPermanent => {
                            {
                                let mut perms = self.permissions.write().await;
                                if let Some(ref cmd) = bash_command {
                                    // For bash, allow the specific command
                                    perms.allow_command_permanently(cmd);
                                } else {
                                    perms.allow_permanently(name);
                                }
                            }
                            // TODO: Persist to config
                            tool.execute(args, ctx).await
                        }
                    }
                } else {
                    Err(ToolError::PermissionDenied(
                        "Approval required but no handler registered".to_string(),
                    ))
                }
            }
            PermissionStatus::Denied(reason) => Err(ToolError::PermissionDenied(reason)),
        }
    }

    pub fn list_tools(&self) -> Vec<&dyn Tool> {
        self.tools.values().map(|t| t.as_ref()).collect()
    }

    pub async fn set_tool_mode(&self, mode: ToolMode) {
        self.permissions.write().await.set_mode(mode);
    }

    pub async fn tool_mode(&self) -> ToolMode {
        self.permissions.read().await.mode()
    }

    pub fn with_builtins(mode: ToolMode) -> Self {
        let mut orch = Self::new(mode);
        orch.register_tool(Box::new(builtin::ReadTool));
        orch.register_tool(Box::new(builtin::WriteTool));
        orch.register_tool(Box::new(builtin::EditTool));
        orch.register_tool(Box::new(builtin::GlobTool));
        orch.register_tool(Box::new(builtin::GrepTool));
        orch.register_tool(Box::new(builtin::BashTool));
        // Note: DiscoverTool requires semantic search backend (not yet implemented)
        orch
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
    fn test_permission_matrix_agi() {
        let matrix = PermissionMatrix::new(ToolMode::Agi);
        let tool = MockTool {
            name: "test".into(),
            danger: DangerLevel::Restricted,
        };
        assert_eq!(matrix.check_permission(&tool), PermissionStatus::Allowed);
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
    fn test_permission_matrix_write_restricted_needs_approval() {
        let matrix = PermissionMatrix::new(ToolMode::Write);
        let tool = MockTool {
            name: "test".into(),
            danger: DangerLevel::Restricted,
        };
        assert_eq!(
            matrix.check_permission(&tool),
            PermissionStatus::NeedsApproval
        );
    }

    #[test]
    fn test_permission_matrix_write_restricted_allowed_session() {
        let mut matrix = PermissionMatrix::new(ToolMode::Write);
        matrix.allow_session("test");
        let tool = MockTool {
            name: "test".into(),
            danger: DangerLevel::Restricted,
        };
        assert_eq!(matrix.check_permission(&tool), PermissionStatus::Allowed);
    }
}
