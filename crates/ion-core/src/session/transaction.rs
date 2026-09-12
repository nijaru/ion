use std::collections::HashSet;

use crate::conversation::context::validate_fork_cutoff;
use crate::session::command::{ConversationSpec, EntryRequest, InputRequest, SessionError, TaskRequest};
use crate::store::{SessionState, apply_mutation};
use crate::view::{Change, CommitEvent};
use crate::{
    CommitSeq, Conversation, ConversationId, Entry, EntryId, Input, InputDisposition, InputId,
    LocalSeq, TaskId, TaskRecord,
};

#[derive(Debug, Clone)]
pub(crate) enum Mutation {
    CreateRoot(Conversation),
    CreateConversation(Conversation),
    AppendEntry(Entry),
    AdmitInput(Input),
    CreateTask(TaskRecord),
    AttachOwnedConversation {
        task_id: TaskId,
        conversation_id: ConversationId,
    },
}

#[derive(Debug)]
pub(crate) struct MutationBatch {
    pub(crate) commit_seq: CommitSeq,
    pub(crate) last_seq: LocalSeq,
    pub(crate) mutations: Vec<Mutation>,
    changes: Vec<Change>,
}

impl MutationBatch {
    pub(crate) fn event(&self) -> CommitEvent {
        CommitEvent {
            commit_seq: self.commit_seq,
            changes: self.changes.clone(),
        }
    }
}

pub(crate) struct Transaction {
    draft: SessionState,
    mutations: Vec<Mutation>,
    changes: Vec<Change>,
}

impl Transaction {
    pub(crate) fn new(state: &SessionState) -> Self {
        Self {
            draft: state.clone(),
            mutations: Vec::new(),
            changes: Vec::new(),
        }
    }

    pub(crate) fn create_root(&mut self) -> Result<ConversationId, SessionError> {
        if self.draft.root_conversation.is_some() {
            return Err(SessionError::Invariant("root conversation already exists".to_owned()));
        }
        let id = ConversationId::new(self.allocate()?.get())?;
        let conversation = Conversation::root(id);
        self.stage(Mutation::CreateRoot(conversation), Change::RootCreated(id))?;
        Ok(id)
    }

    pub(crate) fn create_conversation(
        &mut self,
        spec: ConversationSpec,
    ) -> Result<ConversationId, SessionError> {
        if let Some(parent) = spec.parent {
            let visible = self
                .draft
                .visible_entries(parent.conversation_id)
                .map_err(map_state)?;
            validate_fork_cutoff(&visible, parent.at)?;
        }
        if let Some(owner_task) = spec.owner_task {
            if !self.draft.tasks.contains_key(&owner_task) {
                return Err(SessionError::UnknownTask(owner_task));
            }
        }

        let id = ConversationId::new(self.allocate()?.get())?;
        let conversation = Conversation {
            id,
            parent: spec.parent,
            owner_task: spec.owner_task,
        };
        self.stage(
            Mutation::CreateConversation(conversation),
            Change::ConversationCreated(id),
        )?;
        if let Some(task_id) = spec.owner_task {
            self.stage(
                Mutation::AttachOwnedConversation {
                    task_id,
                    conversation_id: id,
                },
                Change::ConversationOwned {
                    task_id,
                    conversation_id: id,
                },
            )?;
        }
        Ok(id)
    }

    pub(crate) fn append_entry(&mut self, request: EntryRequest) -> Result<EntryId, SessionError> {
        if !self.draft.conversations.contains_key(&request.conversation_id) {
            return Err(SessionError::UnknownConversation(request.conversation_id));
        }
        let visible = self
            .draft
            .visible_entries(request.conversation_id)
            .map_err(map_state)?;
        let visible_ids: HashSet<_> = visible.iter().map(|entry| entry.id).collect();
        if let Some(head) = request.context.head {
            if !visible_ids.contains(&head) {
                return Err(SessionError::InvisibleContextReference(head));
            }
        }
        for edit in &request.context.edits {
            if !visible_ids.contains(&edit.target()) {
                return Err(SessionError::InvisibleContextReference(edit.target()));
            }
        }

        let id = EntryId::new(self.allocate()?.get())?;
        let entry = Entry::new(
            id,
            request.conversation_id,
            request.kind,
            request.data,
            request.projection,
            request.context,
        );
        self.stage(Mutation::AppendEntry(entry), Change::EntryAppended(id))?;
        Ok(id)
    }

    pub(crate) fn create_task(&mut self, request: TaskRequest) -> Result<TaskId, SessionError> {
        if !self.draft.conversations.contains_key(&request.conversation_id) {
            return Err(SessionError::UnknownConversation(request.conversation_id));
        }
        let mut dependencies = HashSet::with_capacity(request.dependencies.len());
        for dependency in &request.dependencies {
            if !dependencies.insert(*dependency) {
                return Err(SessionError::DuplicateDependency(*dependency));
            }
            if !self.draft.tasks.contains_key(dependency) {
                return Err(SessionError::UnknownTask(*dependency));
            }
        }

        let id = TaskId::new(self.allocate()?.get())?;
        let task = TaskRecord::pending(
            id,
            request.conversation_id,
            request.kind,
            request.schema_version,
            request.input,
            request.dependencies,
        );
        self.stage(Mutation::CreateTask(task), Change::TaskCreated(id))?;
        Ok(id)
    }

    pub(crate) fn admit_input(&mut self, request: InputRequest) -> Result<InputId, SessionError> {
        if !self.draft.conversations.contains_key(&request.target) {
            return Err(SessionError::UnknownConversation(request.target));
        }
        let id = InputId::new(self.allocate()?.get())?;
        let input = Input {
            id,
            target: request.target,
            sender: request.sender,
            mode: request.mode,
            request_key: request.request_key,
            body: request.body,
            disposition: InputDisposition::Queued,
        };
        self.stage(Mutation::AdmitInput(input), Change::InputAdmitted(id))?;
        Ok(id)
    }

    pub(crate) fn finish(mut self) -> Result<MutationBatch, SessionError> {
        let last_seq = self.allocate()?;
        let commit_seq = CommitSeq::new(last_seq.get())?;
        Ok(MutationBatch {
            commit_seq,
            last_seq,
            mutations: self.mutations,
            changes: self.changes,
        })
    }

    fn allocate(&mut self) -> Result<LocalSeq, SessionError> {
        let next = match self.draft.last_seq {
            Some(current) => current.next()?,
            None => LocalSeq::new(1)?,
        };
        self.draft.last_seq = Some(next);
        Ok(next)
    }

    fn stage(&mut self, mutation: Mutation, change: Change) -> Result<(), SessionError> {
        apply_mutation(&mut self.draft, &mutation).map_err(map_state)?;
        self.mutations.push(mutation);
        self.changes.push(change);
        Ok(())
    }
}

fn map_state(error: crate::store::StateError) -> SessionError {
    match error {
        crate::store::StateError::UnknownConversation(id) => SessionError::UnknownConversation(id),
        crate::store::StateError::UnknownTask(id) => SessionError::UnknownTask(id),
        other => SessionError::Invariant(other.to_string()),
    }
}
