//! Semantic tool activity and presentation metadata, independent of terminal painting.
use ion_ai::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolActivityKind {
    Read,
    List,
    Search,
    Edit,
    Write,
    Command,
    Ask,
    Subagent,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPresentationTarget {
    None,
    ToolName,
    Argument(String),
    Static(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPresentation {
    pub kind: ToolActivityKind,
    pub target: ToolPresentationTarget,
}

impl ToolPresentation {
    pub fn argument(kind: ToolActivityKind, key: impl Into<String>) -> Self {
        Self {
            kind,
            target: ToolPresentationTarget::Argument(key.into()),
        }
    }

    pub fn static_target(kind: ToolActivityKind, target: impl Into<String>) -> Self {
        Self {
            kind,
            target: ToolPresentationTarget::Static(target.into()),
        }
    }

    pub fn external() -> Self {
        Self {
            kind: ToolActivityKind::External,
            target: ToolPresentationTarget::ToolName,
        }
    }

    pub(super) fn resolve(&self, call: &ToolCall) -> ToolActivity {
        let subject = match &self.target {
            ToolPresentationTarget::None => None,
            ToolPresentationTarget::ToolName => Some(call.name.clone()),
            ToolPresentationTarget::Argument(key) => call
                .arguments
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned),
            ToolPresentationTarget::Static(value) => Some(value.clone()),
        };
        ToolActivity {
            kind: self.kind,
            subject,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolActivity {
    pub kind: ToolActivityKind,
    pub subject: Option<String>,
}

impl ToolActivity {
    pub fn external(name: impl Into<String>) -> Self {
        Self {
            kind: ToolActivityKind::External,
            subject: Some(name.into()),
        }
    }
}
