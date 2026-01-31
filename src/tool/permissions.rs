use crate::tool::types::{DangerLevel, Tool, ToolMode};
use std::collections::HashSet;

/// Manages permissions for tool execution.
#[derive(Debug, Clone, Default)]
pub struct PermissionMatrix {
    mode: ToolMode,
    session_allowed_tools: HashSet<String>,
    permanent_allowed_tools: HashSet<String>,
    /// Per-command bash approvals (session)
    session_allowed_commands: HashSet<String>,
    /// Per-command bash approvals (permanent)
    permanent_allowed_commands: HashSet<String>,
}

impl PermissionMatrix {
    #[must_use]
    pub fn new(mode: ToolMode) -> Self {
        Self {
            mode,
            session_allowed_tools: HashSet::new(),
            permanent_allowed_tools: HashSet::new(),
            session_allowed_commands: HashSet::new(),
            permanent_allowed_commands: HashSet::new(),
        }
    }

    #[must_use]
    pub fn mode(&self) -> ToolMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: ToolMode) {
        self.mode = mode;
    }

    pub fn allow_session(&mut self, tool_name: &str) {
        self.session_allowed_tools.insert(tool_name.to_string());
    }

    pub fn allow_permanently(&mut self, tool_name: &str) {
        self.permanent_allowed_tools.insert(tool_name.to_string());
    }

    /// Allow a specific bash command for this session.
    pub fn allow_command_session(&mut self, command: &str) {
        self.session_allowed_commands.insert(command.to_string());
    }

    /// Allow a specific bash command permanently.
    pub fn allow_command_permanently(&mut self, command: &str) {
        self.permanent_allowed_commands.insert(command.to_string());
    }

    /// Check if a bash command is allowed.
    #[must_use]
    pub fn check_command_permission(&self, command: &str) -> PermissionStatus {
        match self.mode {
            ToolMode::Agi => PermissionStatus::Allowed,
            ToolMode::Read => {
                PermissionStatus::Denied("Bash commands are blocked in Read mode".to_string())
            }
            ToolMode::Write => {
                if self.session_allowed_commands.contains(command)
                    || self.permanent_allowed_commands.contains(command)
                {
                    PermissionStatus::Allowed
                } else {
                    PermissionStatus::NeedsApproval
                }
            }
        }
    }

    pub fn check_permission(&self, tool: &dyn Tool) -> PermissionStatus {
        match self.mode {
            ToolMode::Agi => PermissionStatus::Allowed,
            ToolMode::Read => {
                if tool.danger_level() == DangerLevel::Safe {
                    PermissionStatus::Allowed
                } else {
                    PermissionStatus::Denied("Mutations are blocked in Read mode".to_string())
                }
            }
            ToolMode::Write => {
                if tool.danger_level() == DangerLevel::Safe
                    || tool.name() == "write"
                    || tool.name() == "edit"
                    || self.session_allowed_tools.contains(tool.name())
                    || self.permanent_allowed_tools.contains(tool.name())
                {
                    PermissionStatus::Allowed
                } else {
                    PermissionStatus::NeedsApproval
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PermissionStatus {
    Allowed,
    NeedsApproval,
    Denied(String),
}
