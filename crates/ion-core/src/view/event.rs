use serde::{Deserialize, Serialize};

use crate::{CommitSeq, ConversationId, EntryId, InputId, InvocationKind, TaskId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    RootCreated(ConversationId),
    ConversationCreated(ConversationId),
    EntryAppended(EntryId),
    InputAdmitted(InputId),
    InputDispositionChanged(InputId),
    TaskCreated(TaskId),
    TaskReserved {
        task_id: TaskId,
        generation: u64,
        kind: InvocationKind,
    },
    TaskCheckpointed {
        task_id: TaskId,
        generation: u64,
    },
    TaskCancellationMarked(TaskId),
    TaskSettled(TaskId),
    ConversationOwned {
        task_id: TaskId,
        conversation_id: ConversationId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitEvent {
    pub commit_seq: CommitSeq,
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationBatch {
    pub reset_required: bool,
    pub events: Vec<CommitEvent>,
}
