//! Conversation identity, history ancestry and configuration pointer.

use serde::{Deserialize, Serialize};

use crate::{CommitSeq, ConversationId, EntryId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub history_parent: Option<HistoryParent>,
    pub current_config_revision: CommitSeq,
    pub retired: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryParent {
    pub conversation: ConversationId,
    /// The child sees source history only through this complete visible entry.
    pub at: EntryId,
}
