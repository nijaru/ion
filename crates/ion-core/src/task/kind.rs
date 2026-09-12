use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use thiserror::Error;

use super::{AbortContext, TaskContext, TaskKindName, TaskOutput};
use crate::{CommitSeq, ConversationId, InvocationKind, TaskId, TaskOutcome, TaskOutcomeKind};

pub type TaskFuture<'a> =
    Pin<Box<dyn Future<Output = Result<TaskCompletion, TaskRunError>> + Send + 'a>>;

pub trait TaskKind: Send + Sync {
    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a>;

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a>;

    fn abort<'a>(&'a self, task: RunningTask, context: AbortContext) -> TaskFuture<'a>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunningTask {
    pub id: TaskId,
    pub conversation_id: ConversationId,
    pub kind: TaskKindName,
    pub schema_version: u32,
    pub input: Value,
    pub checkpoint: Option<Value>,
    pub output: Option<TaskOutput>,
    pub generation: u64,
    pub invocation_kind: InvocationKind,
    pub reservation_commit: CommitSeq,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskCompletion {
    pub outcome: TaskOutcome,
    pub output: Option<TaskOutput>,
}

impl TaskCompletion {
    #[must_use]
    pub fn completed(value: Value) -> Self {
        Self::terminal(TaskOutcomeKind::Completed, value)
    }

    #[must_use]
    pub fn aborted(value: Value) -> Self {
        Self::terminal(TaskOutcomeKind::Aborted, value)
    }

    #[must_use]
    pub fn failed(value: Value) -> Self {
        Self::terminal(TaskOutcomeKind::Failed, value)
    }

    #[must_use]
    pub fn unsupported(value: Value) -> Self {
        Self::terminal(TaskOutcomeKind::Unsupported, value)
    }

    #[must_use]
    pub fn terminal(kind: TaskOutcomeKind, value: Value) -> Self {
        Self {
            outcome: TaskOutcome { kind, value },
            output: None,
        }
    }

    #[must_use]
    pub fn with_output(mut self, output: TaskOutput) -> Self {
        self.output = Some(output);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct TaskRunError {
    message: String,
}

impl TaskRunError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<super::TaskContextError> for TaskRunError {
    fn from(error: super::TaskContextError) -> Self {
        Self::new(error.to_string())
    }
}
