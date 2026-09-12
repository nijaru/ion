use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::TaskOutput;
use crate::{ConversationId, TaskId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: TaskId,
    pub conversation_id: ConversationId,
    pub kind: TaskKindName,
    pub schema_version: u32,
    pub input: Value,
    pub checkpoint: Option<Value>,
    pub dependencies: Vec<TaskId>,
    pub generation: u64,
    pub cancel_requested: bool,
    pub status: TaskStatus,
    pub output: Option<TaskOutput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Running,
    Terminal(TaskOutcome),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub kind: TaskOutcomeKind,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskOutcomeKind {
    Completed,
    Failed,
    Aborted,
    Indeterminate,
    Orphaned,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TaskKindName(String);

impl TaskKindName {
    pub fn new(value: impl Into<String>) -> Result<Self, TaskKindNameError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(TaskKindNameError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TaskKindName {
    type Error = TaskKindNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<TaskKindName> for String {
    fn from(value: TaskKindName) -> Self {
        value.0
    }
}

impl fmt::Display for TaskKindName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("task kind name cannot be empty")]
pub struct TaskKindNameError;
