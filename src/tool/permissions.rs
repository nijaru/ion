use crate::tool::types::{DangerLevel, Tool, ToolMode};
use std::collections::HashSet;

/// Manages permissions for tool execution.
#[derive(Debug, Clone, Default)]
pub struct PermissionMatrix {
    mode: ToolMode,
    session_allowed_tools: HashSet<String>,
    permanent_allowed_tools: HashSet<String>,
}

impl PermissionMatrix {
    pub fn new(mode: ToolMode) -> Self {
        Self {
            mode,
            session_allowed_tools: HashSet::new(),
            permanent_allowed_tools: HashSet::new(),
        }
    }

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
