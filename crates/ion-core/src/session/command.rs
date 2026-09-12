use ion_ai::Message;
use serde_json::Value;
use thiserror::Error;

use crate::conversation::context::{ContextControl, ForkError};
use crate::{
    CommitSeq, ConversationId, EntryId, EntryKind, HistoryParent, IdError, InputBody, InputMode,
    InputSender, RequestKey, TaskId, TaskKindName,
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
    pub input_id: crate::InputId,
    pub commit_seq: CommitSeq,
    pub replayed: bool,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("unknown conversation {0}")]
    UnknownConversation(ConversationId),
    #[error("unknown task {0}")]
    UnknownTask(TaskId),
    #[error("context control references entry {0}, which is not visible to the conversation")]
    InvisibleContextReference(EntryId),
    #[error("task dependency {0} appears more than once")]
    DuplicateDependency(TaskId),
    #[error("request key {0} is already bound to different input content or routing")]
    IdempotencyConflict(RequestKey),
    #[error(transparent)]
    InvalidFork(#[from] ForkError),
    #[error(transparent)]
    Id(#[from] IdError),
    #[error("session invariant failed: {0}")]
    Invariant(String),
}
