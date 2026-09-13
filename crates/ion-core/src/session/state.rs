use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use thiserror::Error;

use crate::session::transaction::Mutation;
use crate::{
    CommitSeq, Conversation, ConversationId, Entry, EntryId, Input, InputDisposition, InputId,
    InvocationKind, LocalSeq, RequestKey, SessionId, TaskId, TaskInvocation, TaskKindName,
    TaskRecord, TaskStatus,
};

/// Resident semantic state. Records are held behind `Arc` so a transaction
/// draft clones map structure without copying record payloads; a mutation only
/// deep-copies the records it actually touches (copy-on-write). K4 replaces the
/// remaining per-commit map clone with typed indexed storage reads.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionState {
    pub(crate) session_id: SessionId,
    pub(crate) last_seq: Option<LocalSeq>,
    pub(crate) last_commit: Option<CommitSeq>,
    pub(crate) root_conversation: Option<ConversationId>,
    pub(crate) conversations: BTreeMap<ConversationId, Arc<Conversation>>,
    pub(crate) entries: BTreeMap<EntryId, Arc<Entry>>,
    pub(crate) inputs: BTreeMap<InputId, Arc<Input>>,
    pub(crate) request_keys: HashMap<RequestKey, InputId>,
    pub(crate) input_commits: HashMap<InputId, CommitSeq>,
    pub(crate) tasks: BTreeMap<TaskId, Arc<TaskRecord>>,
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

    /// Whether every fixed dependency of `task` is durably terminal.
    fn dependencies_terminal(&self, task: &TaskRecord) -> bool {
        task.dependencies.iter().all(|dependency| {
            self.tasks
                .get(dependency)
                .is_some_and(|task| matches!(task.status, TaskStatus::Terminal(_)))
        })
    }

    /// Pending work that a settlement just made runnable: successors the plan
    /// created, plus dependents of the settled task whose dependencies are now
    /// all terminal.
    ///
    /// Deliberately scoped to what this settlement touched. An unrelated
    /// pending task that was admitted but never driven stays pending, so
    /// admitting work still never starts it. Whether a candidate is actually
    /// driven also depends on the driver's registered kinds; this is the
    /// readiness predicate, not the dispatch decision. Payloads are not cloned.
    pub(crate) fn runnable_successors(
        &self,
        settled: TaskId,
        created: &[TaskId],
    ) -> Vec<RunnableTask> {
        self.tasks
            .values()
            .filter(|task| {
                matches!(task.status, TaskStatus::Pending)
                    && !task.cancel_requested
                    && (created.contains(&task.id) || task.dependencies.contains(&settled))
                    && self.dependencies_terminal(task)
            })
            .map(|task| RunnableTask {
                id: task.id,
                kind: task.kind.clone(),
                schema_version: task.schema_version,
            })
            .collect()
    }

    /// Terminal, or cancelled, or ready to run: the condition a task wait
    /// resolves on, and the reason a settlement can unblock dependents.
    pub(crate) fn ready(&self, task: &TaskRecord) -> bool {
        matches!(task.status, TaskStatus::Terminal(_))
            || task.cancel_requested
            || self.dependencies_terminal(task)
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
                .map(|entry| (**entry).clone()),
        );
        Ok(visible)
    }
}

/// A pending task whose fixed dependencies are all terminal. Deliberately
/// carries no payload: dispatch decisions do not need to clone task state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunnableTask {
    pub(crate) id: TaskId,
    pub(crate) kind: TaskKindName,
    pub(crate) schema_version: u32,
}

fn conversation_mut(state: &mut SessionState, id: ConversationId) -> Option<&mut Conversation> {
    state.conversations.get_mut(&id).map(Arc::make_mut)
}

fn task_mut(state: &mut SessionState, id: TaskId) -> Option<&mut TaskRecord> {
    state.tasks.get_mut(&id).map(Arc::make_mut)
}

fn input_mut(state: &mut SessionState, id: InputId) -> Option<&mut Input> {
    state.inputs.get_mut(&id).map(Arc::make_mut)
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
                .insert(conversation.id, Arc::new(*conversation))
                .is_some()
            {
                return Err(StateError::DuplicateConversation(conversation.id));
            }
            state.root_conversation = Some(conversation.id);
        }
        Mutation::CreateConversation(conversation) => {
            if state
                .conversations
                .insert(conversation.id, Arc::new(*conversation))
                .is_some()
            {
                return Err(StateError::DuplicateConversation(conversation.id));
            }
        }
        Mutation::SetConversationRetired {
            conversation_id,
            retired,
        } => retire_conversation(state, *conversation_id, *retired)?,
        Mutation::AppendEntry(entry) => {
            ensure_accepts_work(state, entry.conversation_id)?;
            if state
                .entries
                .insert(entry.id, Arc::new(entry.clone()))
                .is_some()
            {
                return Err(StateError::DuplicateEntry(entry.id));
            }
        }
        Mutation::AdmitInput(input) => {
            ensure_accepts_work(state, input.target)?;
            if state
                .inputs
                .insert(input.id, Arc::new(input.clone()))
                .is_some()
            {
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
            ensure_accepts_work(state, task.conversation_id)?;
            if state
                .tasks
                .insert(task.id, Arc::new(task.clone()))
                .is_some()
            {
                return Err(StateError::DuplicateTask(task.id));
            }
        }
        Mutation::OpenForegroundTurn {
            conversation_id,
            task_id,
        } => {
            ensure_accepts_work(state, *conversation_id)?;
            let task = state
                .tasks
                .get(task_id)
                .ok_or(StateError::UnknownTask(*task_id))?;
            if task.conversation_id != *conversation_id || task.turn != Some(*task_id) {
                return Err(StateError::InvalidForegroundTurn(*task_id));
            }
            let conversation = conversation_mut(state, *conversation_id)
                .ok_or(StateError::UnknownConversation(*conversation_id))?;
            if conversation.foreground_turn.is_some() {
                return Err(StateError::ForegroundTurnBusy(*conversation_id));
            }
            conversation.foreground_turn = Some(*task_id);
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
            let task = task_mut(state, *task_id).ok_or(StateError::UnknownTask(*task_id))?;
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
        Mutation::ReleaseForegroundTurn {
            conversation_id,
            task_id,
        } => {
            let conversation = conversation_mut(state, *conversation_id)
                .ok_or(StateError::UnknownConversation(*conversation_id))?;
            if conversation.foreground_turn != Some(*task_id) {
                return Err(StateError::InvalidForegroundTurn(*task_id));
            }
            conversation.foreground_turn = None;
        }
        Mutation::AttachOwnedConversation {
            task_id,
            conversation_id,
        } => {
            let task = task_mut(state, *task_id).ok_or(StateError::UnknownTask(*task_id))?;
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

fn ensure_accepts_work(
    state: &SessionState,
    conversation_id: ConversationId,
) -> Result<(), StateError> {
    let conversation = state
        .conversations
        .get(&conversation_id)
        .ok_or(StateError::UnknownConversation(conversation_id))?;
    if conversation.retired {
        return Err(StateError::ConversationRetired(conversation_id));
    }
    Ok(())
}

/// Retire or reactivate a conversation.
///
/// Retirement is a worker-lifetime operation on an owned conversation, and it
/// requires quiescence: no foreground slot and no non-terminal task. Work that
/// was queued but never started is cancelled in the same commit, because
/// retirement stops future work. Everything already durable - history,
/// ownership, terminal outcomes and checkpoints - is preserved, and an inherited
/// cutoff in another conversation is unaffected.
fn retire_conversation(
    state: &mut SessionState,
    conversation_id: ConversationId,
    retired: bool,
) -> Result<(), StateError> {
    let conversation = state
        .conversations
        .get(&conversation_id)
        .ok_or(StateError::UnknownConversation(conversation_id))?;
    if conversation.retired == retired {
        return Ok(());
    }
    if !retired {
        conversation_mut(state, conversation_id)
            .expect("validated conversation remains present")
            .retired = false;
        return Ok(());
    }
    if conversation.owner_task.is_none() {
        return Err(StateError::ConversationNotOwned(conversation_id));
    }
    if conversation.foreground_turn.is_some()
        || state.tasks.values().any(|task| {
            task.conversation_id == conversation_id
                && !matches!(task.status, TaskStatus::Terminal(_))
        })
    {
        return Err(StateError::ConversationHasLiveWork(conversation_id));
    }

    let queued: Vec<InputId> = state
        .inputs
        .values()
        .filter(|input| {
            input.target == conversation_id && input.disposition == InputDisposition::Queued
        })
        .map(|input| input.id)
        .collect();
    for input_id in queued {
        apply_input_disposition(state, input_id, InputDisposition::Cancelled)?;
    }
    conversation_mut(state, conversation_id)
        .expect("validated conversation remains present")
        .retired = true;
    Ok(())
}

fn apply_input_disposition(
    state: &mut SessionState,
    input_id: InputId,
    disposition: InputDisposition,
) -> Result<(), StateError> {
    let (target, current) = {
        let input = state
            .inputs
            .get(&input_id)
            .ok_or(StateError::UnknownInput(input_id))?;
        (input.target, input.disposition)
    };
    if current == disposition {
        return Ok(());
    }

    let valid_transition = matches!(
        (&current, &disposition),
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
            if task.conversation_id != target {
                return Err(StateError::InvalidInputDisposition(input_id));
            }
        }
        InputDisposition::Consumed(entry_id) => {
            let entry = state
                .entries
                .get(&entry_id)
                .ok_or(StateError::InvisibleParentCutoff(entry_id))?;
            if entry.conversation_id != target {
                return Err(StateError::InvalidInputDisposition(input_id));
            }
        }
        InputDisposition::Queued | InputDisposition::Cancelled => {}
    }

    input_mut(state, input_id)
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
    let task = Arc::make_mut(task);
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
    let task = task_mut(state, task_id).ok_or(StateError::UnknownTask(task_id))?;
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
    #[error(
        "task {task_id} invocation generation {generation} is stale; current generation is {current}"
    )]
    StaleInvocation {
        task_id: TaskId,
        generation: u64,
        current: u64,
    },
    #[error("task {0} normal invocation is fenced by durable cancellation")]
    CancellationFence(TaskId),
    #[error("task {0} invocation generation space is exhausted")]
    GenerationExhausted(TaskId),
    #[error("conversation {0} already has a live foreground turn")]
    ForegroundTurnBusy(ConversationId),
    #[error("task {0} is not a valid foreground turn root")]
    InvalidForegroundTurn(TaskId),
    #[error("task {0} is running without invocation metadata")]
    MissingInvocation(TaskId),
    #[error("input {0} cannot make the requested disposition transition")]
    InvalidInputDisposition(InputId),
    #[error("conversation {0} is retired and accepts no new work")]
    ConversationRetired(ConversationId),
    #[error("conversation {0} has live work and cannot be retired")]
    ConversationHasLiveWork(ConversationId),
    #[error("conversation {0} is not an owned worker and cannot be retired")]
    ConversationNotOwned(ConversationId),
}
