use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    CommitSeq, ConversationId, EntryId, EntryPage, InputId, TaskId, TaskKindName, TaskOutcome,
    TaskOutput,
};

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

    /// Capture the transcript boundary and the placed inputs of this invocation
    /// in one read, before any entry is paged.
    fn request_basis<'a>(
        &'a self,
        task_id: TaskId,
    ) -> ContextFuture<'a, Result<RequestBasis, TaskContextError>>;

    /// One bounded page inside a previously captured basis.
    fn request_entries<'a>(
        &'a self,
        task_id: TaskId,
        basis: RequestBasis,
        after: Option<EntryId>,
        limit: usize,
    ) -> ContextFuture<'a, Result<EntryPage, TaskContextError>>;

    /// The committed outcomes of this invocation's fixed dependencies.
    fn dependency_outcomes<'a>(
        &'a self,
        task_id: TaskId,
    ) -> ContextFuture<'a, Result<Vec<DependencyOutcome>, TaskContextError>>;
}

/// The durable transcript boundary a request froze before it read any entry.
///
/// `Empty` and "unbounded" are different facts: a request that saw no history
/// saw no history even after the conversation appends, so recovery cannot widen
/// it by re-reading current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextCut {
    Empty,
    Through(EntryId),
}

impl ContextCut {
    /// The boundary entry, or `None` when the request saw no history at all.
    #[must_use]
    pub const fn entry(self) -> Option<EntryId> {
        match self {
            Self::Empty => None,
            Self::Through(entry) => Some(entry),
        }
    }
}

/// An accepted input whose placed entry a request's transcript included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacedInput {
    pub input: InputId,
    pub entry: EntryId,
}

/// Everything a request freezes before it pages a single entry: which
/// conversation it reads, how far that history reaches, and which placed inputs
/// it answers. Capturing these together is what makes an append that lands
/// before the first page unable to join the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestBasis {
    pub conversation_id: ConversationId,
    pub cut: ContextCut,
    pub placed: Vec<PlacedInput>,
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

    /// Capture the transcript boundary and placed inputs this invocation reads
    /// at, in one read.
    ///
    /// The returned basis is then passed to [`Self::request_entries`] for every
    /// page, so history appended after this call cannot join the request even
    /// though the pages are read later and outside mutation authority.
    pub async fn request_basis(&self) -> Result<RequestBasis, TaskContextError> {
        self.runtime.request_basis(self.task_id).await
    }

    /// Read one bounded page inside a captured [`RequestBasis`].
    ///
    /// `after` is exclusive and must lie inside the basis' bounded view; `None`
    /// starts at the beginning of it. The read holds the mutation line only for
    /// the read itself, and the caller decides what to do with a long history
    /// instead of receiving an unbounded clone.
    pub async fn request_entries(
        &self,
        basis: &RequestBasis,
        after: Option<EntryId>,
        limit: usize,
    ) -> Result<EntryPage, TaskContextError> {
        self.runtime
            .request_entries(self.task_id, basis.clone(), after, limit)
            .await
    }

    /// The committed outcomes of this invocation's fixed dependencies, in
    /// dependency order. This is what lets a continuation observe the work it
    /// was created to join.
    pub async fn dependency_outcomes(&self) -> Result<Vec<DependencyOutcome>, TaskContextError> {
        self.runtime.dependency_outcomes(self.task_id).await
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
