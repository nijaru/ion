use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{CommitSeq, EntryId, EntryPage, Input, TaskId, TaskKindName, TaskOutcome, TaskOutput};

pub(crate) type ContextFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The resolved durable result of one dependency, as read by an invocation.
///
/// Dependencies are terminal before an invocation is reserved, so this is a
/// read of committed state rather than a wait.
#[derive(Debug, Clone, PartialEq)]
pub struct DependencyOutcome {
    pub task_id: TaskId,
    pub kind: TaskKindName,
    pub outcome: TaskOutcome,
    pub output: Option<TaskOutput>,
}

pub(crate) trait TaskRuntime: Send + Sync {
    fn checkpoint<'a>(
        &'a self,
        task_id: TaskId,
        generation: u64,
        checkpoint: Option<Value>,
        output: Option<TaskOutput>,
    ) -> ContextFuture<'a, Result<CommitSeq, TaskContextError>>;

    /// One bounded page of the invocation conversation's fork-visible entries.
    fn conversation_entries<'a>(
        &'a self,
        task_id: TaskId,
        after: Option<EntryId>,
        limit: usize,
    ) -> ContextFuture<'a, Result<EntryPage, TaskContextError>>;

    /// The committed outcomes of this invocation's fixed dependencies.
    fn dependency_outcomes<'a>(
        &'a self,
        task_id: TaskId,
    ) -> ContextFuture<'a, Result<Vec<DependencyOutcome>, TaskContextError>>;

    /// The inputs durably bound to this invocation's task, in admission order.
    fn placed_inputs<'a>(
        &'a self,
        task_id: TaskId,
    ) -> ContextFuture<'a, Result<Vec<Input>, TaskContextError>>;
}

#[derive(Clone)]
pub struct TaskContext {
    runtime: Arc<dyn TaskRuntime>,
    task_id: TaskId,
    generation: u64,
    cancellation: CancellationToken,
}

impl TaskContext {
    pub(crate) fn new(
        runtime: Arc<dyn TaskRuntime>,
        task_id: TaskId,
        generation: u64,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            runtime,
            task_id,
            generation,
            cancellation,
        }
    }

    pub async fn checkpoint(
        &self,
        checkpoint: Option<Value>,
        output: Option<TaskOutput>,
    ) -> Result<CommitSeq, TaskContextError> {
        self.runtime
            .checkpoint(self.task_id, self.generation, checkpoint, output)
            .await
    }

    /// Read one bounded page of this invocation's conversation transcript.
    ///
    /// `after` is exclusive; `None` starts at the beginning of the fork-visible
    /// range. The read holds the mutation line only for the read itself, and the
    /// caller decides what to do with a long history instead of receiving an
    /// unbounded clone.
    pub async fn conversation_entries(
        &self,
        after: Option<EntryId>,
        limit: usize,
    ) -> Result<EntryPage, TaskContextError> {
        self.runtime
            .conversation_entries(self.task_id, after, limit)
            .await
    }

    /// The committed outcomes of this invocation's fixed dependencies, in
    /// dependency order. This is what lets a continuation observe the work it
    /// was created to join.
    pub async fn dependency_outcomes(&self) -> Result<Vec<DependencyOutcome>, TaskContextError> {
        self.runtime.dependency_outcomes(self.task_id).await
    }

    /// The admitted inputs durably bound to this invocation's task, in
    /// admission order.
    ///
    /// The binding is the task's own `Assigned` disposition, so this cannot read
    /// an unrelated or unbound input. A kind that answers no input gets an empty
    /// list, and every returned input can be consumed by the same settlement.
    /// The accepted inputs whose placed entries this task's turn answers.
    ///
    /// Placement happens when the input is bound to its turn, so this is
    /// provenance rather than a queue: the entry, not this read, is what the
    /// model request is built from.
    pub async fn placed_inputs(&self) -> Result<Vec<Input>, TaskContextError> {
        self.runtime.placed_inputs(self.task_id).await
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }
}

#[derive(Clone)]
pub struct AbortContext {
    runtime: Arc<dyn TaskRuntime>,
    task_id: TaskId,
    generation: u64,
}

impl AbortContext {
    pub(crate) fn new(runtime: Arc<dyn TaskRuntime>, task_id: TaskId, generation: u64) -> Self {
        Self {
            runtime,
            task_id,
            generation,
        }
    }

    pub async fn checkpoint(
        &self,
        checkpoint: Option<Value>,
        output: Option<TaskOutput>,
    ) -> Result<CommitSeq, TaskContextError> {
        self.runtime
            .checkpoint(self.task_id, self.generation, checkpoint, output)
            .await
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaskContextError {
    #[error("task invocation was cancelled")]
    Cancelled,
    #[error("task invocation is stale")]
    Stale,
    #[error("session is closed")]
    Closed,
    #[error("session persistence failed: {0}")]
    Persistence(String),
    #[error("task runtime rejected the commit: {0}")]
    Runtime(String),
}
