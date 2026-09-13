pub mod context;
mod entry;
mod input;

pub use entry::{Entry, EntryKind, EntryKindError};
pub use input::{
    Input, InputBody, InputDisposition, InputMode, InputSender, RequestKey, RequestKeyError,
};

use serde::{Deserialize, Serialize};

use crate::{ConversationId, EntryId, TaskId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub parent: Option<HistoryParent>,
    pub owner_task: Option<TaskId>,
    /// The non-terminal foreground turn root, if any. One authoritative slot per
    /// conversation; background work and retained workers never occupy it.
    pub foreground_turn: Option<TaskId>,
    /// Retired conversations are read-only archives: history, ownership and
    /// terminal work are preserved, and no writer may add work to them.
    pub retired: bool,
}

impl Conversation {
    #[must_use]
    pub const fn root(id: ConversationId) -> Self {
        Self {
            id,
            parent: None,
            owner_task: None,
            foreground_turn: None,
            retired: false,
        }
    }

    #[must_use]
    pub const fn owned(
        id: ConversationId,
        owner_task: TaskId,
        parent: Option<HistoryParent>,
    ) -> Self {
        Self {
            id,
            parent,
            owner_task: Some(owner_task),
            foreground_turn: None,
            retired: false,
        }
    }

    /// Whether this conversation accepts new work.
    #[must_use]
    pub const fn accepts_work(&self) -> bool {
        !self.retired
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryParent {
    pub conversation_id: ConversationId,
    pub at: EntryId,
}
