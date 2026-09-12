mod memory;

pub(crate) use memory::MemoryStore;

use std::collections::{BTreeMap, HashMap};

use thiserror::Error;

use crate::session::transaction::Mutation;
use crate::{
    CommitSeq, Conversation, ConversationId, Entry, EntryId, Input, InputDisposition, InputId,
    InvocationKind, LocalSeq, RequestKey, SessionId, TaskId, TaskInvocation, TaskRecord, TaskStatus,
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
        Mutation::SetInputDisposition {
            input_id,
            disposition,
        } => apply_input_disposition(state, *input_id, *disposition)?,
        Mutation::CreateTask(task) => {
            if state.tasks.insert(task.id, task.clone()).is_some() {
                return Err(StateError::DuplicateTask(task.id));
            }
        }
        Mutation::ReserveTask {
            task_id,
            generation,
            kind,
        } => reserve_task(state, *task_id, *generation, *kind)?,
        Mutation::CheckpointTask {
            task_id,
            generation,
            checkpoint,
            output,
        } => {
            let task = authorized_task_mut(state, *task_id, *generation)?;
            task.checkpoint = checkpoint.clone();
            task.output = output.clone();
        }
        Mutation::MarkTaskCancellation(task_id) => {
            let task = state
                .tasks
                .get_mut(task_id)
                .ok_or(StateError::UnknownTask(*task_id))?;
            if matches!(task.status, TaskStatus::Terminal(_)) {
                return Err(StateError::TaskAlreadyTerminal(*task_id));
            }
            task.cancel_requested = true;
        }
        Mutation::SettleTask {
            task_id,
            generation,
            outcome,
            output,
        } => {
            let task = authorized_task_mut(state, *task_id, *generation)?;
            task.status = TaskStatus::Terminal(outcome.clone());
            task.invocation = None;
            task.output = output.clone();
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

fn apply_input_disposition(
    state: &mut SessionState,
    input_id: InputId,
    disposition: InputDisposition,
) -> Result<(), StateError> {
    let input = state
        .inputs
        .get(&input_id)
        .ok_or(StateError::UnknownInput(input_id))?;
    if input.disposition == disposition {
        return Ok(());
    }

    let valid_transition = matches!(
        (&input.disposition, &disposition),
        (InputDisposition::Queued, InputDisposition::Assigned(_))
            | (InputDisposition::Queued, InputDisposition::Consumed(_))
            | (InputDisposition::Queued, InputDisposition::Cancelled)
            | (InputDisposition::Assigned(_), InputDisposition::Consumed(_))
            | (InputDisposition::Assigned(_), InputDisposition::Cancelled)
    );
    if !valid_transition {
        return Err(StateError::InvalidInputDisposition(input_id));
    }

    match disposition {
        InputDisposition::Assigned(task_id) => {
            let task = state
                .tasks
                .get(&task_id)
                .ok_or(StateError::UnknownTask(task_id))?;
            if task.conversation_id != input.target {
                return Err(StateError::InvalidInputDisposition(input_id));
            }
        }
        InputDisposition::Consumed(entry_id) => {
            let entry = state
                .entries
                .get(&entry_id)
                .ok_or(StateError::InvisibleParentCutoff(entry_id))?;
            if entry.conversation_id != input.target {
                return Err(StateError::InvalidInputDisposition(input_id));
            }
        }
        InputDisposition::Queued | InputDisposition::Cancelled => {}
    }

    state
        .inputs
        .get_mut(&input_id)
        .expect("validated input remains present")
        .disposition = disposition;
    Ok(())
}

fn reserve_task(
    state: &mut SessionState,
    task_id: TaskId,
    generation: u64,
    kind: InvocationKind,
) -> Result<(), StateError> {
    let task = state
        .tasks
        .get(&task_id)
        .ok_or(StateError::UnknownTask(task_id))?;
    let expected_generation = task
        .generation
        .checked_add(1)
        .ok_or(StateError::GenerationExhausted(task_id))?;
    if generation != expected_generation {
        return Err(StateError::StaleInvocation {
            task_id,
            generation,
            current: task.generation,
        });
    }

    match kind {
        InvocationKind::Execute => {
            if !matches!(task.status, TaskStatus::Pending) {
                return Err(StateError::TaskNotPending(task_id));
            }
            if task.cancel_requested {
                return Err(StateError::CancellationFence(task_id));
            }
            if task.dependencies.iter().any(|dependency| {
                !matches!(
                    state.tasks.get(dependency).map(|task| &task.status),
                    Some(TaskStatus::Terminal(_))
                )
            }) {
                return Err(StateError::DependenciesNotReady(task_id));
            }
        }
        InvocationKind::Recover => {
            if !matches!(task.status, TaskStatus::Running) || task.cancel_requested {
                return Err(StateError::InvalidInvocationKind { task_id, kind });
            }
        }
        InvocationKind::Abort => {
            if matches!(task.status, TaskStatus::Terminal(_)) || !task.cancel_requested {
                return Err(StateError::InvalidInvocationKind { task_id, kind });
            }
        }
    }

    let task = state
        .tasks
        .get_mut(&task_id)
        .expect("validated task remains present");
    task.generation = generation;
    task.invocation = Some(TaskInvocation { generation, kind });
    task.status = TaskStatus::Running;
    Ok(())
}

fn authorized_task_mut(
    state: &mut SessionState,
    task_id: TaskId,
    generation: u64,
) -> Result<&mut TaskRecord, StateError> {
    let task = state
        .tasks
        .get_mut(&task_id)
        .ok_or(StateError::UnknownTask(task_id))?;
    if !matches!(task.status, TaskStatus::Running) {
        return Err(StateError::TaskNotRunning(task_id));
    }
    if task.generation != generation {
        return Err(StateError::StaleInvocation {
            task_id,
            generation,
            current: task.generation,
        });
    }
    let invocation = task
        .invocation
        .ok_or(StateError::MissingInvocation(task_id))?;
    if task.cancel_requested && invocation.kind != InvocationKind::Abort {
        return Err(StateError::CancellationFence(task_id));
    }
    Ok(task)
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum StateError {
    #[error("unknown conversation {0}")]
    UnknownConversation(ConversationId),
    #[error("unknown input {0}")]
    UnknownInput(InputId),
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
    #[error("task {0} has dependencies that are not terminal")]
    DependenciesNotReady(TaskId),
    #[error("task {0} is not pending")]
    TaskNotPending(TaskId),
    #[error("task {0} is not running")]
    TaskNotRunning(TaskId),
    #[error("task {0} is already terminal")]
    TaskAlreadyTerminal(TaskId),
    #[error("task {task_id} cannot reserve {kind:?} in its current state")]
    InvalidInvocationKind {
        task_id: TaskId,
        kind: InvocationKind,
    },
    #[error("task {task_id} invocation generation {generation} is stale; current generation is {current}")]
    StaleInvocation {
        task_id: TaskId,
        generation: u64,
        current: u64,
    },
    #[error("task {0} normal invocation is fenced by durable cancellation")]
    CancellationFence(TaskId),
    #[error("task {0} invocation generation space is exhausted")]
    GenerationExhausted(TaskId),
    #[error("task {0} is running without invocation metadata")]
    MissingInvocation(TaskId),
    #[error("input {0} cannot make the requested disposition transition")]
    InvalidInputDisposition(InputId),
}
