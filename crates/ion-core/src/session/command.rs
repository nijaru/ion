use ion_ai::Message;
use serde_json::Value;
use thiserror::Error;

use crate::conversation::context::{ContextControl, ForkError};
use crate::{
    CommitSeq, ConversationId, EntryId, EntryKind, HistoryParent, IdError, InputBody, InputId,
    InputMode, InputSender, InvocationKind, RequestKey, TaskId, TaskKindName,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConversationSpec {
    pub parent: Option<HistoryParent>,
    pub owner_task: Option<TaskId>,
}

impl ConversationSpec {
    #[must_use]
    pub const fn independent() -> Self {
        Self {
            parent: None,
            owner_task: None,
        }
    }

    #[must_use]
    pub const fn fork(parent: ConversationId, at: EntryId) -> Self {
        Self {
            parent: Some(HistoryParent {
                conversation_id: parent,
                at,
            }),
            owner_task: None,
        }
    }

    #[must_use]
    pub const fn owned(owner_task: TaskId, parent: Option<HistoryParent>) -> Self {
        Self {
            parent,
            owner_task: Some(owner_task),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EntryRequest {
    pub conversation_id: ConversationId,
    pub kind: EntryKind,
    pub data: Value,
    pub projection: Vec<Message>,
    pub context: ContextControl,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskRequest {
    pub conversation_id: ConversationId,
    pub kind: TaskKindName,
    pub schema_version: u32,
    pub input: Value,
    pub dependencies: Vec<TaskId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRequest {
    pub target: ConversationId,
    pub sender: InputSender,
    pub mode: InputMode,
    pub request_key: Option<RequestKey>,
    pub body: InputBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationReceipt {
    pub conversation_id: ConversationId,
    pub commit_seq: CommitSeq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryReceipt {
    pub entry_id: EntryId,
    pub commit_seq: CommitSeq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskReceipt {
    pub task_id: TaskId,
    pub commit_seq: CommitSeq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputReceipt {
    pub input_id: InputId,
    pub commit_seq: CommitSeq,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvocationReceipt {
    pub(crate) generation: u64,
    pub(crate) kind: InvocationKind,
    pub(crate) commit_seq: CommitSeq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CancellationReceipt {
    pub(crate) changed: bool,
    pub(crate) commit_seq: CommitSeq,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("unknown conversation {0}")]
    UnknownConversation(ConversationId),
    #[error("unknown input {0}")]
    UnknownInput(InputId),
    #[error("unknown task {0}")]
    UnknownTask(TaskId),
    #[error("context control references entry {0}, which is not visible to the conversation")]
    InvisibleContextReference(EntryId),
    #[error("task dependency {0} appears more than once")]
    DuplicateDependency(TaskId),
    #[error("task {0} has dependencies that are not terminal")]
    DependenciesNotReady(TaskId),
    #[error("task {0} is not pending")]
    TaskNotPending(TaskId),
    #[error("task {0} is not running")]
    TaskNotRunning(TaskId),
    #[error("task {0} is already terminal")]
    TaskAlreadyTerminal(TaskId),
    #[error("task {task_id} cannot reserve {kind:?} in its current state")]
    InvalidInvocationKind {
        task_id: TaskId,
        kind: InvocationKind,
    },
    #[error(
        "task {task_id} invocation generation {generation} is stale; current generation is {current}"
    )]
    StaleInvocation {
        task_id: TaskId,
        generation: u64,
        current: u64,
    },
    #[error("task {0} normal invocation is fenced by durable cancellation")]
    CancellationFence(TaskId),
    #[error("task {0} invocation generation space is exhausted")]
    GenerationExhausted(TaskId),
    #[error("input {0} cannot make the requested disposition transition")]
    InvalidInputDisposition(InputId),
    #[error("request key {0} is already bound to different input content or routing")]
    IdempotencyConflict(RequestKey),
    #[error(transparent)]
    InvalidFork(#[from] ForkError),
    #[error(transparent)]
    Id(#[from] IdError),
    #[error("session invariant failed: {0}")]
    Invariant(String),
}
