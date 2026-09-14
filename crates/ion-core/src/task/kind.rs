use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use thiserror::Error;

use super::{AbortContext, TaskContext, TaskKindName, TaskOutput, TaskPlan};
use crate::{CommitSeq, ConversationId, InvocationKind, TaskId, TaskOutcome, TaskOutcomeKind};

pub type TaskFuture<'a> =
    Pin<Box<dyn Future<Output = Result<TaskCompletion, TaskRunError>> + Send + 'a>>;

/// A single scarce resource held only while an eligible invocation runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceDomain {
    Model,
    Tool,
    Process,
}

pub trait TaskKind: Send + Sync {
    fn resource_domain(&self) -> Option<ResourceDomain> {
        None
    }

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

#[derive(Debug, PartialEq)]
pub struct TaskCompletion {
    pub outcome: TaskOutcome,
    pub output: Option<TaskOutput>,
    /// Canonical successor writes committed atomically with this outcome.
    pub plan: TaskPlan,
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

    /// Terminal outcome for work whose external effect may have happened but
    /// cannot be safely reconciled. Retain the evidence in `value`.
    #[must_use]
    pub fn indeterminate(value: Value) -> Self {
        Self::terminal(TaskOutcomeKind::Indeterminate, value)
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
            plan: TaskPlan::default(),
        }
    }

    #[must_use]
    pub fn with_output(mut self, output: TaskOutput) -> Self {
        self.output = Some(output);
        self
    }

    /// Commit the plan atomically with this outcome. The writer revalidates
    /// invocation generation, cancellation and authority before applying it.
    #[must_use]
    pub fn with_plan(mut self, plan: TaskPlan) -> Self {
        self.plan = plan;
        self
    }
}

/// An interrupted invocation, not a terminal application outcome. The durable
/// task remains recoverable. Use `TaskCompletion::failed` for a known failure
/// with no unresolved external action, or Indeterminate with retained evidence.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaskRunError {
    #[error("task invocation interrupted: {0}")]
    Interrupted(String),
    #[error(transparent)]
    Runtime(#[from] super::TaskContextError),
}

impl TaskRunError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self::Interrupted(message.into())
    }
}
