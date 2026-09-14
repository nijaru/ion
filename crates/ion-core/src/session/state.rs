use std::collections::btree_set::BTreeSet;
use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::sync::Arc;

use thiserror::Error;

use crate::session::transaction::Mutation;
use crate::{
    CommitSeq, Conversation, ConversationId, Entry, EntryId, Input, InputDisposition, InputId,
    InputPlacement, InvocationKind, LocalSeq, RequestKey, SessionId, TaskId, TaskInvocation,
    TaskKindName, TaskRecord, TaskStatus,
};

/// Resident semantic state. Records are held behind `Arc` so a transaction
/// draft clones map structure without copying record payloads; a mutation only
/// deep-copies the records it actually touches (copy-on-write). R6 replaces the
/// remaining per-commit map clone and the whole-history scans with indexes and
/// typed indexed storage reads.
///
/// `entries_by_conversation` is a derived index of `entries`: it answers "which
/// entries belong to this conversation, in order" without scanning every entry
/// in the session. Derived state is maintained in exactly one place
/// ([`apply_mutation`]) so it cannot drift from the records it indexes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionState {
    pub(crate) session_id: SessionId,
    pub(crate) last_seq: Option<LocalSeq>,
    pub(crate) last_commit: Option<CommitSeq>,
    pub(crate) root_conversation: Option<ConversationId>,
    pub(crate) conversations: BTreeMap<ConversationId, Arc<Conversation>>,
    pub(crate) entries: BTreeMap<EntryId, Arc<Entry>>,
    pub(crate) entries_by_conversation: BTreeMap<ConversationId, BTreeSet<EntryId>>,
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
            entries_by_conversation: BTreeMap::new(),
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

    /// Store one entry and index it under its conversation.
    ///
    /// This is the only place the entry index is written, so the index and the
    /// records it describes cannot disagree. Reconstruction uses it too: loading
    /// a retired conversation's history is not new work, so it does not go
    /// through the acceptance check that [`apply_mutation`] applies to a live
    /// append.
    pub(crate) fn insert_entry(&mut self, entry: Entry) -> Result<(), StateError> {
        let conversation_id = entry.conversation_id;
        let entry_id = entry.id;
        if self.entries.insert(entry_id, Arc::new(entry)).is_some() {
            return Err(StateError::DuplicateEntry(entry_id));
        }
        self.entries_by_conversation
            .entry(conversation_id)
            .or_default()
            .insert(entry_id);
        Ok(())
    }

    /// The fork-visible entry ids of one conversation, in order, without
    /// cloning the entries they identify.
    ///
    /// A paging reader needs the order and the membership; only the page it
    /// returns needs payloads. Ids are eight bytes, so this costs the transcript
    /// length in ids rather than in records, and the ancestry of the
    /// conversation rather than the size of the session.
    pub(crate) fn visible_ids(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<EntryId>, StateError> {
        let conversation = self
            .conversations
            .get(&conversation_id)
            .ok_or(StateError::UnknownConversation(conversation_id))?;
        let own = self
            .entries_by_conversation
            .get(&conversation_id)
            .into_iter()
            .flat_map(|ids| ids.iter().copied());
        match conversation.parent {
            None => Ok(own.collect()),
            Some(parent) => {
                let cutoff = parent.at;
                let inherited = self.visible_ids(parent.conversation_id)?;
                let mut visible = Vec::with_capacity(inherited.len() + 1);
                let mut found = false;
                for id in inherited {
                    visible.push(id);
                    if id == cutoff {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err(StateError::InvisibleParentCutoff(cutoff));
                }
                visible.extend(own);
                Ok(visible)
            }
        }
    }

    /// One entry record, without materializing an index or a transcript.
    pub(crate) fn entry(&self, entry_id: EntryId) -> Option<&Entry> {
        self.entries.get(&entry_id).map(|entry| &**entry)
    }

    /// One page of a conversation's fork-visible entry order.
    ///
    /// `after` is exclusive. Returns the page and whether more entries follow.
    ///
    /// Cost: for a conversation with no history parent, one ordered range read
    /// over its own index, so the page costs the page rather than the
    /// transcript. A fork must also reproduce its inherited prefix, so its page
    /// costs its ancestry plus the page; resolving a cursor inside that prefix
    /// is why the prefix is materialized as ids, which is eight bytes per
    /// inherited entry rather than a cloned record.
    pub(crate) fn visible_page(
        &self,
        conversation_id: ConversationId,
        after: Option<EntryId>,
        limit: usize,
    ) -> Result<(Vec<EntryId>, bool), StateError> {
        let conversation = self
            .conversations
            .get(&conversation_id)
            .ok_or(StateError::UnknownConversation(conversation_id))?;
        if limit == 0 {
            // An empty request has no continuation cursor to offer: there is no
            // entry to hand back as the next page's `after`.
            return Ok((Vec::new(), false));
        }
        let empty = BTreeSet::new();
        let own = self
            .entries_by_conversation
            .get(&conversation_id)
            .unwrap_or(&empty);
        let Some(parent) = conversation.parent else {
            let mut page: Vec<EntryId> = match after {
                Some(cursor) => {
                    if !own.contains(&cursor) {
                        return Err(StateError::InvisibleCursor(cursor));
                    }
                    own.range((Bound::Excluded(cursor), Bound::Unbounded))
                        .take(limit + 1)
                        .copied()
                        .collect()
                }
                None => own.iter().take(limit + 1).copied().collect(),
            };
            let more = page.len() > limit;
            page.truncate(limit);
            return Ok((page, more));
        };

        // Fork: the visible order is the inherited prefix followed by own
        // entries. Materialize the prefix as ids (see above) and satisfy the
        // cursor wherever it lands.
        let inherited = self.visible_ids(parent.conversation_id)?;
        let mut prefix = Vec::new();
        let mut found_cutoff = false;
        for id in inherited {
            prefix.push(id);
            if id == parent.at {
                found_cutoff = true;
                break;
            }
        }
        if !found_cutoff {
            return Err(StateError::InvisibleParentCutoff(parent.at));
        }

        let mut page = Vec::new();
        let mut more = false;
        match after {
            None => {
                for id in &prefix {
                    if page.len() == limit {
                        more = true;
                        break;
                    }
                    page.push(*id);
                }
                if !more {
                    let taken = limit - page.len();
                    let mut rest: Vec<EntryId> = own.iter().take(taken + 1).copied().collect();
                    more = rest.len() > taken;
                    rest.truncate(taken);
                    page.extend(rest);
                }
            }
            Some(cursor) => {
                let position = prefix.iter().position(|id| *id == cursor);
                match position {
                    Some(position) => {
                        for id in prefix.iter().skip(position + 1) {
                            if page.len() == limit {
                                more = true;
                                break;
                            }
                            page.push(*id);
                        }
                        if !more {
                            let taken = limit - page.len();
                            let mut rest: Vec<EntryId> =
                                own.iter().take(taken + 1).copied().collect();
                            more = rest.len() > taken;
                            rest.truncate(taken);
                            page.extend(rest);
                        }
                    }
                    None if own.contains(&cursor) => {
                        page.extend(
                            own.range((Bound::Excluded(cursor), Bound::Unbounded))
                                .take(limit + 1)
                                .copied(),
                        );
                        more = page.len() > limit;
                        page.truncate(limit);
                    }
                    None => return Err(StateError::InvisibleCursor(cursor)),
                }
            }
        }
        Ok((page, more))
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

        if let Some(ids) = self.entries_by_conversation.get(&conversation_id) {
            visible.extend(
                ids.iter()
                    .filter_map(|id| self.entries.get(id))
                    .map(|entry| {
                        // The index and the record map are written together in
                        // `apply_mutation`, so a miss here would mean the index drifted.
                        (**entry).clone()
                    }),
            );
        }
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
        Mutation::CloseTurn { root, closed_by } => {
            let closed = state
                .tasks
                .get(closed_by)
                .ok_or(StateError::UnknownTask(*closed_by))?;
            if !matches!(closed.status, TaskStatus::Terminal(_)) || closed.turn != Some(*root) {
                return Err(StateError::InvalidTurnClosure(*root));
            }
            let root_task = task_mut(state, *root).ok_or(StateError::UnknownTask(*root))?;
            if root_task.turn != Some(*root) || root_task.turn_closed_by.is_some() {
                return Err(StateError::InvalidTurnClosure(*root));
            }
            root_task.turn_closed_by = Some(*closed_by);
        }
        Mutation::AppendEntry(entry) => {
            ensure_accepts_work(state, entry.conversation_id)?;
            state.insert_entry(entry.clone())?;
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
        Mutation::MarkTurnCancelled {
            conversation_id,
            root,
        } => {
            let conversation = conversation_mut(state, *conversation_id)
                .ok_or(StateError::UnknownConversation(*conversation_id))?;
            if conversation.foreground_turn != Some(*root) {
                return Err(StateError::InvalidForegroundTurn(*root));
            }
            conversation.turn_cancelled = true;
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
            // The turn is over, so its barrier must not reach a later turn in
            // this conversation.
            conversation.turn_cancelled = false;
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
/// Check that a placement names real history and a real answering turn.
///
/// The turn must be a root in the input's conversation, and the entry must be the
/// content this input's body places: a binding to an unrelated entry would make
/// "answering that input" a claim about text the model never saw.
fn validate_placement(
    state: &SessionState,
    input_id: InputId,
    target: ConversationId,
    current: &InputDisposition,
    placement: &InputPlacement,
) -> Result<(), StateError> {
    let task = state
        .tasks
        .get(&placement.turn)
        .ok_or(StateError::UnknownTask(placement.turn))?;
    if task.conversation_id != target || task.turn != Some(placement.turn) {
        return Err(StateError::InvalidInputDisposition(input_id));
    }
    let entry = state
        .entries
        .get(&placement.entry)
        .ok_or(StateError::InvisibleParentCutoff(placement.entry))?;
    if entry.conversation_id != target {
        return Err(StateError::InvalidInputDisposition(input_id));
    }
    let input = state
        .inputs
        .get(&input_id)
        .ok_or(StateError::UnknownInput(input_id))?;
    let placed = input.body.placement();
    if entry.kind != placed.kind || entry.data != placed.data {
        return Err(StateError::InvalidInputDisposition(input_id));
    }
    // An existing placement keeps its entry: a retry answers the same message
    // rather than pointing the binding at some other entry.
    if let Some(previous) = current.placement()
        && previous.entry != placement.entry
    {
        return Err(StateError::InvalidInputDisposition(input_id));
    }
    Ok(())
}

/// The conversation's inputs that were admitted and never started.
pub(crate) fn queued_inputs(state: &SessionState, conversation_id: ConversationId) -> Vec<InputId> {
    state
        .inputs
        .values()
        .filter(|input| {
            input.target == conversation_id && input.disposition == InputDisposition::Queued
        })
        .map(|input| input.id)
        .collect()
}

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

    // The transaction stages these cancellations explicitly so they are durable;
    // applying them here as well keeps the resident invariant true for any caller
    // that applies the mutation without staging them.
    for input_id in queued_inputs(state, conversation_id) {
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

    // Queued input may be placed or withdrawn. Placed content is never erased:
    // a placed input can move to another attempt or be abandoned, but not back to
    // `Queued` and not to a payload-free `Cancelled` that would lose its entry.
    let valid_transition = matches!(
        (&current, &disposition),
        (InputDisposition::Queued, InputDisposition::Placed(_))
            | (InputDisposition::Queued, InputDisposition::Cancelled)
            | (InputDisposition::Placed(_), InputDisposition::Placed(_))
            | (InputDisposition::Placed(_), InputDisposition::Abandoned(_))
            | (InputDisposition::Abandoned(_), InputDisposition::Placed(_))
    );
    if !valid_transition {
        return Err(StateError::InvalidInputDisposition(input_id));
    }

    match disposition {
        InputDisposition::Placed(placement) | InputDisposition::Abandoned(placement) => {
            validate_placement(state, input_id, target, &current, &placement)?;
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
    #[error("entry {0} is not visible in this conversation")]
    InvisibleCursor(EntryId),
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
    #[error("turn {0} cannot be closed by that settlement")]
    InvalidTurnClosure(TaskId),
}
