use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{CommitSeq, TaskId, TaskOutput};

pub(crate) type ContextFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) trait TaskRuntime: Send + Sync {
    fn checkpoint<'a>(
        &'a self,
        task_id: TaskId,
        generation: u64,
        checkpoint: Option<Value>,
        output: Option<TaskOutput>,
    ) -> ContextFuture<'a, Result<CommitSeq, TaskContextError>>;
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
