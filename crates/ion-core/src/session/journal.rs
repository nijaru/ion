//! Uncommitted changes to resident state, and how to take them back.
//!
//! A command prepares its writes directly on the resident state instead of
//! copying it. One process owns a loaded session while a command runs, no reader
//! can observe the state before the commit is durable, and a rejected command
//! rolls back through this journal. Copying the resident maps per commit made
//! every command cost the whole history before anything was persisted.
//!
//! Every write a prepared command makes goes through [`Editor`], which records
//! the previous value first. [`Editor::rollback`] replays those records in
//! reverse, so a rejected command leaves resident state exactly as it found it.
//! Nothing here is durable: the write set that reaches the store is built
//! separately by the transaction. A caller must not mutate resident state
//! behind the editor's back, because that write would not be undoable.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::state::{SessionState, StateError};
use crate::{
    CommitSeq, Conversation, ConversationConfig, ConversationId, Entry, EntryId, Input,
    InputDisposition, InputId, LocalSeq, RequestKey, TaskId, TaskRecord,
};

/// The previous value of one write, or the exact index change to invert.
#[derive(Debug)]
enum Undo {
    Conversation(ConversationId, Option<Arc<Conversation>>),
    Entry(EntryId, Option<Arc<Entry>>),
    Input(InputId, Option<Arc<Input>>),
    Task(TaskId, Option<Arc<TaskRecord>>),
    Config(ConversationId, Option<Arc<ConversationConfig>>),
    ConfigRevision(ConversationId, Option<CommitSeq>),
    RequestKey(RequestKey, Option<InputId>),
    InputCommit(InputId, Option<CommitSeq>),
    RootConversation(Option<ConversationId>),
    LastSeq(Option<LocalSeq>),
    LastCommit(Option<CommitSeq>),
    /// Membership added to a conversation's entry index by [`Editor::put_entry`].
    EntryIndex(ConversationId, EntryId),
    /// Membership of the queued-input index, and whether it held the id.
    Queued(InputId, bool),
    /// A turn-membership edge added by [`Editor::put_task`].
    TaskTurn(TaskId, TaskId),
    /// A reverse dependency edge added by [`Editor::put_task`].
    Dependent(TaskId, TaskId),
}

/// Writes to resident state during one uncommitted command.
#[derive(Debug)]
pub(crate) struct Editor<'a> {
    state: &'a mut SessionState,
    undo: Vec<Undo>,
}

impl<'a> Editor<'a> {
    pub(crate) fn new(state: &'a mut SessionState) -> Self {
        Self {
            state,
            undo: Vec::new(),
        }
    }

    /// Resident state as the prepared command currently sees it.
    pub(crate) fn state(&self) -> &SessionState {
        self.state
    }

    pub(crate) fn conversation(&self, id: ConversationId) -> Option<&Conversation> {
        self.state.conversations.get(&id).map(|value| &**value)
    }

    pub(crate) fn task(&self, id: TaskId) -> Option<&TaskRecord> {
        self.state.tasks.get(&id).map(|value| &**value)
    }

    pub(crate) fn input(&self, id: InputId) -> Option<&Input> {
        self.state.inputs.get(&id).map(|value| &**value)
    }

    pub(crate) fn config_revision(&self, id: ConversationId) -> Option<CommitSeq> {
        self.state.config_revisions.get(&id).copied()
    }

    pub(crate) fn root_conversation(&self) -> Option<ConversationId> {
        self.state.root_conversation
    }

    pub(crate) fn last_seq(&self) -> Option<LocalSeq> {
        self.state.last_seq
    }

    /// Copy a record on write, keeping the previous value for rollback.
    ///
    /// This is the same single-record copy the resident state already made per
    /// touched record; what it no longer does is copy every untouched record in
    /// the map as well.
    pub(crate) fn put_conversation(
        &mut self,
        conversation: Conversation,
    ) -> Result<(), StateError> {
        let id = conversation.id;
        if self.state.conversations.contains_key(&id) {
            return Err(StateError::DuplicateConversation(id));
        }
        self.undo.push(Undo::Conversation(id, None));
        self.state.conversations.insert(id, Arc::new(conversation));
        Ok(())
    }

    pub(crate) fn conversation_mut(&mut self, id: ConversationId) -> Option<&mut Conversation> {
        let current = self.state.conversations.get(&id).cloned()?;
        // `Conversation` is small and copyable; the other records are cloned.
        let draft = *current;
        self.undo.push(Undo::Conversation(id, Some(current)));
        self.state.conversations.insert(id, Arc::new(draft));
        self.state.conversations.get_mut(&id).map(Arc::make_mut)
    }

    pub(crate) fn put_entry(&mut self, entry: Entry) -> Result<(), StateError> {
        let id = entry.id;
        let conversation_id = entry.conversation_id;
        self.state.insert_entry(entry)?;
        self.undo.push(Undo::EntryIndex(conversation_id, id));
        self.undo.push(Undo::Entry(id, None));
        Ok(())
    }

    pub(crate) fn put_input(&mut self, input: Input) -> Result<(), StateError> {
        let id = input.id;
        let queued = input.disposition == InputDisposition::Queued;
        self.state.insert_input(input)?;
        if queued {
            self.undo.push(Undo::Queued(id, false));
        }
        self.undo.push(Undo::Input(id, None));
        Ok(())
    }

    /// Record whether an input is queued, keeping the index and its undo in step.
    pub(crate) fn set_queued(&mut self, input_id: InputId, queued: bool) {
        let previous = self.state.queued.contains(&input_id);
        if previous == queued {
            return;
        }
        self.undo.push(Undo::Queued(input_id, previous));
        self.state.set_queued(input_id, queued);
    }

    pub(crate) fn input_mut(&mut self, id: InputId) -> Option<&mut Input> {
        let current = self.state.inputs.get(&id).cloned()?;
        let draft = (*current).clone();
        self.undo.push(Undo::Input(id, Some(current)));
        self.state.inputs.insert(id, Arc::new(draft));
        self.state.inputs.get_mut(&id).map(Arc::make_mut)
    }

    pub(crate) fn put_task(&mut self, task: TaskRecord) -> Result<(), StateError> {
        let id = task.id;
        let turn = task.turn;
        let dependencies = task.dependencies.clone();
        self.state.insert_task(task)?;
        if let Some(turn) = turn {
            self.undo.push(Undo::TaskTurn(turn, id));
        }
        for dependency in dependencies {
            self.undo.push(Undo::Dependent(dependency, id));
        }
        self.undo.push(Undo::Task(id, None));
        Ok(())
    }

    pub(crate) fn task_mut(&mut self, id: TaskId) -> Option<&mut TaskRecord> {
        let current = self.state.tasks.get(&id).cloned()?;
        let draft = (*current).clone();
        self.undo.push(Undo::Task(id, Some(current)));
        self.state.tasks.insert(id, Arc::new(draft));
        self.state.tasks.get_mut(&id).map(Arc::make_mut)
    }

    /// Install a conversation's configuration. The revision is written
    /// separately, when the command's commit sequence is known.
    pub(crate) fn set_config(&mut self, id: ConversationId, config: ConversationConfig) {
        let previous = self.state.configs.insert(id, Arc::new(config));
        self.undo.push(Undo::Config(id, previous));
    }

    /// Bind an installed configuration to the commit that installed it.
    pub(crate) fn set_config_revision(&mut self, id: ConversationId, revision: CommitSeq) {
        let previous = self.state.config_revisions.insert(id, revision);
        self.undo.push(Undo::ConfigRevision(id, previous));
    }

    pub(crate) fn put_request_key(
        &mut self,
        key: RequestKey,
        input_id: InputId,
    ) -> Result<(), StateError> {
        if self.state.request_keys.contains_key(&key) {
            return Err(StateError::DuplicateRequestKey(key));
        }
        self.undo.push(Undo::RequestKey(key.clone(), None));
        self.state.request_keys.insert(key, input_id);
        Ok(())
    }

    pub(crate) fn set_input_commit(&mut self, id: InputId, commit: CommitSeq) {
        let previous = self.state.input_commits.insert(id, commit);
        self.undo.push(Undo::InputCommit(id, previous));
    }

    pub(crate) fn set_root_conversation(&mut self, id: ConversationId) {
        let previous = self.state.root_conversation.replace(id);
        self.undo.push(Undo::RootConversation(previous));
    }

    pub(crate) fn set_last_seq(&mut self, seq: LocalSeq) {
        let previous = self.state.last_seq.replace(seq);
        self.undo.push(Undo::LastSeq(previous));
    }

    pub(crate) fn set_last_commit(&mut self, commit: CommitSeq) {
        let previous = self.state.last_commit.replace(commit);
        self.undo.push(Undo::LastCommit(previous));
    }

    /// Undo every write this editor made, most recent first.
    pub(crate) fn rollback(mut self) {
        while let Some(entry) = self.undo.pop() {
            match entry {
                Undo::Conversation(id, previous) => {
                    restore(&mut self.state.conversations, id, previous);
                }
                Undo::Entry(id, previous) => restore(&mut self.state.entries, id, previous),
                Undo::Input(id, previous) => restore(&mut self.state.inputs, id, previous),
                Undo::Task(id, previous) => restore(&mut self.state.tasks, id, previous),
                Undo::Config(id, previous) => restore(&mut self.state.configs, id, previous),
                Undo::ConfigRevision(id, previous) => {
                    restore_revision(&mut self.state.config_revisions, id, previous);
                }
                Undo::RequestKey(key, previous) => match previous {
                    Some(input_id) => {
                        self.state.request_keys.insert(key, input_id);
                    }
                    None => {
                        self.state.request_keys.remove(&key);
                    }
                },
                Undo::InputCommit(id, previous) => match previous {
                    Some(commit) => {
                        self.state.input_commits.insert(id, commit);
                    }
                    None => {
                        self.state.input_commits.remove(&id);
                    }
                },
                Undo::RootConversation(previous) => self.state.root_conversation = previous,
                Undo::LastSeq(previous) => self.state.last_seq = previous,
                Undo::LastCommit(previous) => self.state.last_commit = previous,
                Undo::EntryIndex(conversation_id, entry_id) => {
                    if let Some(ids) = self.state.entries_by_conversation.get_mut(&conversation_id)
                    {
                        ids.remove(&entry_id);
                        if ids.is_empty() {
                            self.state.entries_by_conversation.remove(&conversation_id);
                        }
                    }
                }
                Undo::Queued(input_id, previous) => self.state.set_queued(input_id, previous),
                Undo::TaskTurn(turn, task_id) => {
                    if let Some(ids) = self.state.tasks_by_turn.get_mut(&turn) {
                        ids.remove(&task_id);
                        if ids.is_empty() {
                            self.state.tasks_by_turn.remove(&turn);
                        }
                    }
                }
                Undo::Dependent(dependency, task_id) => {
                    if let Some(ids) = self.state.dependents.get_mut(&dependency) {
                        ids.remove(&task_id);
                        if ids.is_empty() {
                            self.state.dependents.remove(&dependency);
                        }
                    }
                }
            }
        }
    }
}

fn restore_revision<K: Ord, V>(map: &mut BTreeMap<K, V>, key: K, previous: Option<V>) {
    match previous {
        Some(value) => {
            map.insert(key, value);
        }
        None => {
            map.remove(&key);
        }
    }
}

fn restore<K: Ord, V>(map: &mut BTreeMap<K, Arc<V>>, key: K, previous: Option<Arc<V>>) {
    match previous {
        Some(value) => {
            map.insert(key, value);
        }
        None => {
            map.remove(&key);
        }
    }
}
