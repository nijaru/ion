use std::collections::HashSet;

use super::{Persistence, StoreError};
use crate::session::transaction::{Mutation, MutationBatch};
use crate::{ConversationId, EntryId, InputId, LocalSeq, TaskId};

/// Volatile commit sink that also enforces the durability-boundary invariants a
/// real store must enforce: monotonic commit cursors and at-most-once creation
/// of each typed identity. Semantic validation already happened against the
/// resident draft; this is the persistence-side safety net.
#[derive(Debug, Default)]
pub(crate) struct MemoryStore {
    last_seq: Option<LocalSeq>,
    created_conversations: HashSet<ConversationId>,
    created_entries: HashSet<EntryId>,
    created_inputs: HashSet<InputId>,
    created_tasks: HashSet<TaskId>,
}

impl MemoryStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn guard_creations(&mut self, writes: &[Mutation]) -> Result<(), StoreError> {
        for write in writes {
            let unique = match write {
                Mutation::CreateRoot(conversation) | Mutation::CreateConversation(conversation) => {
                    self.created_conversations.insert(conversation.id)
                }
                Mutation::AppendEntry(entry) => self.created_entries.insert(entry.id),
                Mutation::AdmitInput(input) => self.created_inputs.insert(input.id),
                Mutation::CreateTask(task) => self.created_tasks.insert(task.id),
                Mutation::SetInputDisposition { .. }
                | Mutation::OpenForegroundTurn { .. }
                | Mutation::ReserveTask { .. }
                | Mutation::CheckpointTask { .. }
                | Mutation::MarkTaskCancellation(_)
                | Mutation::SettleTask { .. }
                | Mutation::AttachOwnedConversation { .. }
                | Mutation::MarkTurnCancelled { .. }
                | Mutation::ReleaseForegroundTurn { .. }
                | Mutation::SetConversationRetired { .. }
                | Mutation::CloseTurn { .. } => true,
            };
            if !unique {
                return Err(StoreError::other(
                    "durable write set creates an identity twice".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

impl Persistence for MemoryStore {
    fn commit(&mut self, batch: &MutationBatch) -> Result<(), StoreError> {
        if batch.commit_seq.local_seq() != batch.last_seq
            || self
                .last_seq
                .is_some_and(|current| batch.last_seq <= current)
        {
            return Err(StoreError::other("invalid commit sequence".to_owned()));
        }
        self.guard_creations(&batch.writes)?;
        self.last_seq = Some(batch.last_seq);
        Ok(())
    }
}
