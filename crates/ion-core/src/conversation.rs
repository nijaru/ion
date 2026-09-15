//! Conversations: immutable transcripts with an optional history parent.
//!
//! A conversation is not an agent, a worker or a security boundary. It groups
//! entries and owns an installed configuration; a branch is a conversation, and
//! a worker would also be a conversation. History edges point only at
//! conversations that already existed and at cuts that are visible inside their
//! source, which is what keeps the graph acyclic and every cutoff meaningful.

use serde::{Deserialize, Serialize};

use crate::config::InstalledConfig;
use crate::{ConversationId, EntryId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    /// Where this conversation branched from, if it is not the session root.
    pub parent: Option<HistoryParent>,
    /// The installed configuration, if one was ever committed.
    pub config: Option<InstalledConfig>,
}

impl Conversation {
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.parent.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryParent {
    pub conversation_id: ConversationId,
    /// The last entry of the source conversation that this branch inherits.
    pub at: EntryId,
}
