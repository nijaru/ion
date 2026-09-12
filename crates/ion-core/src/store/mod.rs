mod memory;

pub(crate) use memory::MemoryStore;

use std::collections::{BTreeMap, HashMap};

use thiserror::Error;

use crate::session::transaction::Mutation;
use crate::{
    CommitSeq, Conversation, ConversationId, Entry, EntryId, Input, InputId, LocalSeq, RequestKey,
    SessionId, TaskId, TaskRecord,
};

#[derive(Debug, Clone)]
pub(crate) struct SessionState {
    pub(crate) session_id: SessionId,
    pub(crate) last_seq: Option<LocalSeq>,
    pub(crate) last_commit: Option<CommitSeq>,
    pub(crate) root_conversation: Option<ConversationId>,
    pub(crate) conversations: BTreeMap<ConversationId, Conversation>,
    pub(crate) entries: BTreeMap<EntryId, Entry>,
    pub(crate) inputs: BTreeMap<InputId, Input>,
    pub(crate) request_keys: HashMap<RequestKey, InputId>,
    pub(crate) input_commits: HashMap<InputId, CommitSeq>,
    pub(crate) tasks: BTreeMap<TaskId, TaskRecord>,
}

impl SessionState {
    pub(crate) fn empty(session_id: SessionId) -> Self {
        Self {
            session_id,
            last_seq: None,
            last_commit: None,
            root_conversation: None,
            conversations: BTreeMap::new(),
            entries: BTreeMap::new(),
            inputs: BTreeMap::new(),
            request_keys: HashMap::new(),
            input_commits: HashMap::new(),
            tasks: BTreeMap::new(),
        }
    }

    pub(crate) fn visible_entries(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<Entry>, StateError> {
        let conversation = self
            .conversations
            .get(&conversation_id)
            .ok_or(StateError::UnknownConversation(conversation_id))?;

        let mut visible = if let Some(parent) = conversation.parent {
            let mut inherited = self.visible_entries(parent.conversation_id)?;
            let position = inherited
                .iter()
                .position(|entry| entry.id == parent.at)
                .ok_or(StateError::InvisibleParentCutoff(parent.at))?;
            inherited.truncate(position + 1);
            inherited
        } else {
            Vec::new()
        };

        visible.extend(
            self.entries
                .values()
                .filter(|entry| entry.conversation_id == conversation_id)
                .cloned(),
        );
        Ok(visible)
    }
}

pub(crate) fn apply_mutation(
    state: &mut SessionState,
    mutation: &Mutation,
) -> Result<(), StateError> {
    match mutation {
        Mutation::CreateRoot(conversation) => {
            if state.root_conversation.is_some() {
                return Err(StateError::RootAlreadyExists);
            }
            if state
                .conversations
                .insert(conversation.id, *conversation)
                .is_some()
            {
                return Err(StateError::DuplicateConversation(conversation.id));
            }
            state.root_conversation = Some(conversation.id);
        }
        Mutation::CreateConversation(conversation) => {
            if state
                .conversations
                .insert(conversation.id, *conversation)
                .is_some()
            {
                return Err(StateError::DuplicateConversation(conversation.id));
            }
        }
        Mutation::AppendEntry(entry) => {
            if state.entries.insert(entry.id, entry.clone()).is_some() {
                return Err(StateError::DuplicateEntry(entry.id));
            }
        }
        Mutation::AdmitInput(input) => {
            if state.inputs.insert(input.id, input.clone()).is_some() {
                return Err(StateError::DuplicateInput(input.id));
            }
            if let Some(key) = &input.request_key
                && state.request_keys.insert(key.clone(), input.id).is_some()
            {
                return Err(StateError::DuplicateRequestKey(key.clone()));
            }
        }
        Mutation::CreateTask(task) => {
            if state.tasks.insert(task.id, task.clone()).is_some() {
                return Err(StateError::DuplicateTask(task.id));
            }
        }
        Mutation::AttachOwnedConversation {
            task_id,
            conversation_id,
        } => {
            let task = state
                .tasks
                .get_mut(task_id)
                .ok_or(StateError::UnknownTask(*task_id))?;
            if task.owned_conversations.contains(conversation_id) {
                return Err(StateError::DuplicateOwnership {
                    task_id: *task_id,
                    conversation_id: *conversation_id,
                });
            }
            task.owned_conversations.push(*conversation_id);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum StateError {
    #[error("unknown conversation {0}")]
    UnknownConversation(ConversationId),
    #[error("unknown task {0}")]
    UnknownTask(TaskId),
    #[error("history parent cutoff {0} is not visible")]
    InvisibleParentCutoff(EntryId),
    #[error("session root already exists")]
    RootAlreadyExists,
    #[error("duplicate conversation id {0}")]
    DuplicateConversation(ConversationId),
    #[error("duplicate entry id {0}")]
    DuplicateEntry(EntryId),
    #[error("duplicate input id {0}")]
    DuplicateInput(InputId),
    #[error("duplicate task id {0}")]
    DuplicateTask(TaskId),
    #[error("duplicate request key {0}")]
    DuplicateRequestKey(RequestKey),
    #[error("task {task_id} already owns conversation {conversation_id}")]
    DuplicateOwnership {
        task_id: TaskId,
        conversation_id: ConversationId,
    },
}
