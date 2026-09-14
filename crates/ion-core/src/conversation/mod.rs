pub mod context;
mod entry;
mod input;

pub use entry::{Entry, EntryKind, EntryKindError};
pub use input::{
    EntryPlacement, INPUT_ENTRY, Input, InputBody, InputDisposition, InputMode, InputPlacement,
    InputSender, RequestKey, RequestKeyError,
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
    /// Whether the turn holding that slot has been cancelled.
    ///
    /// Turn cancellation is a property of the turn, not of the root operation:
    /// a root settles as soon as it has planned its children, so a barrier read
    /// from the root's own cancellation state would let cleanup from a live
    /// member create runnable work in a stopped turn. The flag is meaningful
    /// only while `foreground_turn` is set and is cleared when the slot is
    /// released.
    pub turn_cancelled: bool,
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
            turn_cancelled: false,
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
            turn_cancelled: false,
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
