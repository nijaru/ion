use crate::tool::builtin::guard::is_safe_command;
use crate::tool::types::{DangerLevel, Tool, ToolMode};

/// Manages permissions for tool execution.
#[derive(Debug, Clone, Default)]
pub struct PermissionMatrix {
    mode: ToolMode,
}

impl PermissionMatrix {
    #[must_use]
    pub fn new(mode: ToolMode) -> Self {
        Self { mode }
    }

    #[must_use]
    pub fn mode(&self) -> ToolMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: ToolMode) {
        self.mode = mode;
    }

    /// Check if a bash command is allowed.
    #[must_use]
    pub fn check_command_permission(&self, command: &str) -> PermissionStatus {
        match self.mode {
            ToolMode::Write => PermissionStatus::Allowed,
            ToolMode::Read => {
                if is_safe_command(command) {
                    PermissionStatus::Allowed
                } else {
                    PermissionStatus::Denied(
                        "Command blocked in Read mode (not in safe list)".into(),
                    )
                }
            }
        }
    }

    pub fn check_permission(&self, tool: &dyn Tool) -> PermissionStatus {
        match self.mode {
            ToolMode::Write => PermissionStatus::Allowed,
            ToolMode::Read => {
                if tool.danger_level() == DangerLevel::Safe {
                    PermissionStatus::Allowed
                } else {
                    PermissionStatus::Denied("Mutations are blocked in Read mode".to_string())
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PermissionStatus {
    Allowed,
    Denied(String),
}
