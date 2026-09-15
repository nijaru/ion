mod reconstruction;

use std::collections::btree_set::BTreeSet;
use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::sync::Arc;

use thiserror::Error;

use crate::session::journal::Editor;
use crate::session::transaction::Mutation;
use crate::task::ContextCut;
use crate::{
    CommitSeq, Conversation, ConversationConfig, ConversationId, Entry, EntryId, Input,
    InputDisposition, InputId, InputPlacement, InstalledConfig, InvocationKind, LocalSeq,
    RequestKey, SessionId, TaskId, TaskInvocation, TaskKindName, TaskRecord, TaskStatus,
};

/// Resident semantic state. Records are held behind `Arc` so a command writes in
/// place through the rollback journal and copies only the records it touches;
/// the per-commit clone of every map was removed at `54367ada`, and the derived
/// indexes below are the read path that replaced the whole-history scans. What
/// remains open is residency: `open` still materializes every record (R6).
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
    /// Each conversation's installed generation configuration. It lives beside
    /// the conversation record instead of inside it: the record stays a small
    /// copyable value, and a snapshot does not carry every instruction payload.
    pub(crate) configs: BTreeMap<ConversationId, Arc<ConversationConfig>>,
    /// The commit that installed each configuration, and the basis a
    /// reconfiguration compares against. A command's commit sequence is known
    /// only when the command is sealed, so - like an admitted input's commit
    /// binding - it is written then. A configuration always has a revision and a
    /// revision always names a configuration; reconstruction enforces both.
    pub(crate) config_revisions: BTreeMap<ConversationId, CommitSeq>,
    pub(crate) entries: BTreeMap<EntryId, Arc<Entry>>,
    pub(crate) entries_by_conversation: BTreeMap<ConversationId, BTreeSet<EntryId>>,
    /// Inputs whose disposition is `Queued`, in admission order. Scheduling and
    /// retirement ask for queued work often enough that scanning every input ever
    /// admitted is not acceptable.
    pub(crate) queued: BTreeSet<InputId>,
    pub(crate) inputs: BTreeMap<InputId, Arc<Input>>,
    pub(crate) request_keys: HashMap<RequestKey, InputId>,
    pub(crate) input_commits: HashMap<InputId, CommitSeq>,
    pub(crate) tasks: BTreeMap<TaskId, Arc<TaskRecord>>,
    /// Tasks by the turn they belong to, each root included under its own id.
    /// `TaskRecord::turn` is fixed when the task is created, so this index only
    /// changes when a task is inserted.
    pub(crate) tasks_by_turn: BTreeMap<TaskId, BTreeSet<TaskId>>,
    /// Reverse dependency edges: which tasks wait on a given task. Fixed when
    /// the dependent is created, so this index only changes on insertion.
    pub(crate) dependents: BTreeMap<TaskId, BTreeSet<TaskId>>,
}

impl SessionState {
    pub(crate) fn empty(session_id: SessionId) -> Self {
        Self {
            session_id,
            last_seq: None,
            last_commit: None,
            root_conversation: None,
            conversations: BTreeMap::new(),
            configs: BTreeMap::new(),
            config_revisions: BTreeMap::new(),
            entries: BTreeMap::new(),
            entries_by_conversation: BTreeMap::new(),
            queued: BTreeSet::new(),
            tasks_by_turn: BTreeMap::new(),
            dependents: BTreeMap::new(),
            inputs: BTreeMap::new(),
            request_keys: HashMap::new(),
            input_commits: HashMap::new(),
            tasks: BTreeMap::new(),
        }
    }

    /// The configuration installed on one conversation, with its revision.
    ///
    /// Absence is a real answer: an unconfigured conversation is inspectable,
    /// and generation refuses it rather than falling back to a default model.
    #[must_use]
    pub(crate) fn installed_config(&self, id: ConversationId) -> Option<InstalledConfig> {
        let revision = self.config_revisions.get(&id).copied()?;
        let config = self.configs.get(&id)?;
        Some(InstalledConfig::new(revision, (**config).clone()))
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
        // Candidates are exactly what this settlement touched: the plan's
        // successors plus the tasks that depend on it. Scanning every task to
        // find them would make releasing a slot cost the whole history.
        let dependents = self.dependents.get(&settled);
        let candidates = created
            .iter()
            .copied()
            .chain(dependents.into_iter().flat_map(|ids| ids.iter().copied()));
        let mut runnable = Vec::new();
        for candidate in candidates {
            let Some(task) = self.tasks.get(&candidate) else {
                continue;
            };
            if matches!(task.status, TaskStatus::Pending)
                && !task.cancel_requested
                && self.dependencies_terminal(task)
            {
                runnable.push(RunnableTask {
                    id: task.id,
                    kind: task.kind.clone(),
                    schema_version: task.schema_version,
                });
            }
        }
        runnable
    }

    /// Terminal, or cancelled, or ready to run: the condition a task wait
    /// resolves on, and the reason a settlement can unblock dependents.
    pub(crate) fn ready(&self, task: &TaskRecord) -> bool {
        matches!(task.status, TaskStatus::Terminal(_))
            || task.cancel_requested
            || self.dependencies_terminal(task)
    }

    /// Store one input, refusing a duplicate id, and index it if it is queued.
    ///
    /// The rejection is checked before the record is stored, so a duplicate
    /// leaves the resident record and its index exactly as they were; there is
    /// no inverse to record because no write happened.
    pub(crate) fn insert_input(&mut self, input: Input) -> Result<(), StateError> {
        let id = input.id;
        let queued = input.disposition == InputDisposition::Queued;
        if let std::collections::btree_map::Entry::Occupied(_) = self.inputs.entry(id) {
            return Err(StateError::DuplicateInput(id));
        }
        self.inputs.insert(id, Arc::new(input));
        if queued {
            self.queued.insert(id);
        }
        Ok(())
    }

    /// Store one task, refusing a duplicate id, and index its turn and
    /// dependencies. A duplicate leaves the stored task and its indexes
    /// unchanged.
    pub(crate) fn insert_task(&mut self, task: TaskRecord) -> Result<(), StateError> {
        let id = task.id;
        let turn = task.turn;
        let dependencies = task.dependencies.clone();
        if let std::collections::btree_map::Entry::Occupied(_) = self.tasks.entry(id) {
            return Err(StateError::DuplicateTask(id));
        }
        self.tasks.insert(id, Arc::new(task));
        if let Some(turn) = turn {
            self.tasks_by_turn.entry(turn).or_default().insert(id);
        }
        for dependency in dependencies {
            self.dependents.entry(dependency).or_default().insert(id);
        }
        Ok(())
    }

    /// Record whether an input is queued, keeping the scheduling index in step.
    pub(crate) fn set_queued(&mut self, input_id: InputId, queued: bool) {
        if queued {
            self.queued.insert(input_id);
        } else {
            self.queued.remove(&input_id);
        }
    }

    /// The tasks of one turn, root included, as ids.
    pub(crate) fn tasks_of_turn(&self, turn: TaskId) -> Vec<TaskId> {
        self.tasks_by_turn
            .get(&turn)
            .map(|ids| ids.iter().copied().collect())
            .unwrap_or_default()
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
        if let std::collections::btree_map::Entry::Occupied(_) = self.entries.entry(entry_id) {
            return Err(StateError::DuplicateEntry(entry_id));
        }
        self.entries.insert(entry_id, Arc::new(entry));
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

    /// One conversation record.
    pub(crate) fn conversation(&self, conversation_id: ConversationId) -> Option<&Conversation> {
        self.conversations
            .get(&conversation_id)
            .map(|conversation| &**conversation)
    }

    /// The last entry a request over this conversation can see, or `Empty` when
    /// the conversation has no visible history yet.
    ///
    /// This is what a request freezes as its transcript cutoff. A conversation
    /// with no entries of its own starts at `Empty`, and a fork whose parent
    /// supplied nothing still ends at the parent's cutoff rather than at whatever
    /// the parent appended later.
    pub(crate) fn last_visible_id(
        &self,
        conversation_id: ConversationId,
    ) -> Result<ContextCut, StateError> {
        let conversation = self
            .conversations
            .get(&conversation_id)
            .ok_or(StateError::UnknownConversation(conversation_id))?;
        if let Some(last) = self
            .entries_by_conversation
            .get(&conversation_id)
            .and_then(|ids| ids.iter().next_back())
        {
            return Ok(ContextCut::Through(*last));
        }
        match conversation.parent {
            None => Ok(ContextCut::Empty),
            Some(parent) => Ok(ContextCut::Through(parent.at)),
        }
    }

    /// One page of a request's bounded view of a conversation.
    ///
    /// `cut` is the boundary the request froze: `Empty` saw no history at all and
    /// `Through(id)` includes `id` and nothing appended after it, so a later
    /// append cannot leak into a request that already captured its basis. `after`
    /// is exclusive and must lie inside the same bounded view.
    ///
    /// Cost matches [`Self::visible_page`]: the page, plus a fork's ancestry.
    pub(crate) fn request_page(
        &self,
        conversation_id: ConversationId,
        cut: ContextCut,
        after: Option<EntryId>,
        limit: usize,
    ) -> Result<(Vec<EntryId>, bool), StateError> {
        let ContextCut::Through(end) = cut else {
            return Ok((Vec::new(), false));
        };
        if limit == 0 || after == Some(end) {
            // Either nothing was asked for, or the caller already reached the cut.
            return Ok((Vec::new(), false));
        }
        let conversation = self
            .conversations
            .get(&conversation_id)
            .ok_or(StateError::UnknownConversation(conversation_id))?;
        let empty = BTreeSet::new();
        let own = self
            .entries_by_conversation
            .get(&conversation_id)
            .unwrap_or(&empty);

        // The inherited prefix is the parent's visible order up to this fork's
        // cutoff, materialized as ids: eight bytes each rather than records.
        let prefix: Vec<EntryId> = match conversation.parent {
            None => Vec::new(),
            Some(parent) => {
                let mut prefix = Vec::new();
                let mut found = false;
                for id in self.visible_ids(parent.conversation_id)? {
                    prefix.push(id);
                    if id == parent.at {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err(StateError::InvisibleParentCutoff(parent.at));
                }
                prefix
            }
        };
        // Where the cut sits decides which segments this view can contain: a cut
        // inherited from the prefix excludes every own entry, because those come
        // after it in the visible order.
        let cut_in_prefix = prefix.iter().position(|id| *id == end);
        if cut_in_prefix.is_none() && !own.contains(&end) {
            return Err(StateError::InvisibleCursor(end));
        }
        let prefix_view_end = cut_in_prefix.map_or(prefix.len(), |position| position + 1);

        let mut page: Vec<EntryId> = Vec::new();

        // Validate the cursor against this bounded view *before* any range is
        // built: a cursor past the cut is outside the request, and a range with
        // its start beyond its end is a panic rather than an error.
        if let Some(cursor) = after {
            let in_prefix = prefix.iter().position(|id| *id == cursor);
            let valid = match in_prefix {
                Some(position) => cut_in_prefix.is_none_or(|cut| position <= cut),
                None => cut_in_prefix.is_none() && own.contains(&cursor) && cursor < end,
            };
            if !valid {
                return Err(StateError::InvisibleCursor(cursor));
            }
        }
        let resume = match after {
            None => 0,
            Some(cursor) => prefix
                .iter()
                .position(|id| *id == cursor)
                .map_or(prefix_view_end, |position| position + 1),
        };

        for id in prefix.iter().take(prefix_view_end).skip(resume) {
            page.push(*id);
            if *id == end || page.len() > limit {
                break;
            }
        }
        if cut_in_prefix.is_none() && page.last() != Some(&end) && page.len() <= limit {
            let range = match after {
                Some(cursor) if own.contains(&cursor) => {
                    (Bound::Excluded(cursor), Bound::Included(end))
                }
                Some(_) => (Bound::Unbounded, Bound::Included(end)),
                None => (Bound::Unbounded, Bound::Included(end)),
            };
            for id in own.range(range) {
                page.push(*id);
                if *id == end || page.len() > limit {
                    break;
                }
            }
        }

        let more = page.len() > limit;
        if more {
            page.truncate(limit);
        } else if page.last() != Some(&end) {
            // A bounded view that does not reach its own cut is inconsistent.
            return Err(StateError::InvisibleCursor(end));
        }
        Ok((page, more))
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

pub(crate) fn apply_mutation(
    editor: &mut Editor<'_>,
    mutation: &Mutation,
) -> Result<(), StateError> {
    let state = editor.state();
    match mutation {
        Mutation::CreateRoot(conversation) => {
            if state.root_conversation.is_some() {
                return Err(StateError::RootAlreadyExists);
            }
            editor.put_conversation(*conversation)?;
            editor.set_root_conversation(conversation.id);
        }
        Mutation::CreateConversation(conversation) => {
            editor.put_conversation(*conversation)?;
        }
        Mutation::SetConversationConfig {
            conversation_id,
            config,
        } => {
            // Configuring a retired conversation is refused: a retired
            // conversation accepts no new work, so a configuration no
            // generation may read would only be a misleading durable record.
            ensure_accepts_work(state, *conversation_id)?;
            editor.set_config(*conversation_id, config.clone());
        }
        Mutation::SetConversationRetired {
            conversation_id,
            retired,
        } => retire_conversation(editor, *conversation_id, *retired)?,
        Mutation::CloseTurn { root, closed_by } => {
            let closed = editor
                .task(*closed_by)
                .ok_or(StateError::UnknownTask(*closed_by))?;
            if !matches!(closed.status, TaskStatus::Terminal(_)) || closed.turn != Some(*root) {
                return Err(StateError::InvalidTurnClosure(*root));
            }
            let root_task = editor
                .task_mut(*root)
                .ok_or(StateError::UnknownTask(*root))?;
            if root_task.turn != Some(*root) || root_task.turn_closed_by.is_some() {
                return Err(StateError::InvalidTurnClosure(*root));
            }
            root_task.turn_closed_by = Some(*closed_by);
        }
        Mutation::AppendEntry(entry) => {
            ensure_accepts_work(state, entry.conversation_id)?;
            editor.put_entry(entry.clone())?;
        }
        Mutation::AdmitInput(input) => {
            ensure_accepts_work(state, input.target)?;
            editor.put_input(input.clone())?;
            if let Some(key) = &input.request_key {
                editor.put_request_key(key.clone(), input.id)?;
            }
        }
        Mutation::SetInputDisposition {
            input_id,
            disposition,
        } => apply_input_disposition(editor, *input_id, *disposition)?,
        Mutation::CreateTask(task) => {
            ensure_accepts_work(state, task.conversation_id)?;
            editor.put_task(task.clone())?;
        }
        Mutation::OpenForegroundTurn {
            conversation_id,
            task_id,
        } => {
            ensure_accepts_work(state, *conversation_id)?;
            let task = editor
                .task(*task_id)
                .ok_or(StateError::UnknownTask(*task_id))?;
            if task.conversation_id != *conversation_id || task.turn != Some(*task_id) {
                return Err(StateError::InvalidForegroundTurn(*task_id));
            }
            let conversation = editor
                .conversation_mut(*conversation_id)
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
        } => reserve_task(editor, *task_id, *generation, *kind)?,
        Mutation::CheckpointTask {
            task_id,
            generation,
            checkpoint,
            output,
        } => {
            let task = authorized_task_mut(editor, *task_id, *generation)?;
            task.checkpoint = checkpoint.clone();
            task.output = output.clone();
        }
        Mutation::MarkTaskCancellation(task_id) => {
            let task = editor
                .task_mut(*task_id)
                .ok_or(StateError::UnknownTask(*task_id))?;
            if matches!(task.status, TaskStatus::Terminal(_)) {
                return Err(StateError::TaskAlreadyTerminal(*task_id));
            }
            task.cancel_requested = true;
        }
        Mutation::MarkTurnCancelled {
            conversation_id,
            root,
        } => {
            let conversation = editor
                .conversation_mut(*conversation_id)
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
            let task = authorized_task_mut(editor, *task_id, *generation)?;
            task.status = TaskStatus::Terminal(outcome.clone());
            task.invocation = None;
            task.output = output.clone();
        }
        Mutation::ReleaseForegroundTurn {
            conversation_id,
            task_id,
        } => {
            let conversation = editor
                .conversation_mut(*conversation_id)
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
            let task = editor
                .task_mut(*task_id)
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
    // Only queued inputs are candidates, in admission order; the index already
    // excludes every input that has been placed, abandoned or cancelled.
    state
        .queued
        .iter()
        .filter(|input_id| {
            state
                .inputs
                .get(input_id)
                .is_some_and(|input| input.target == conversation_id)
        })
        .copied()
        .collect()
}

/// Retirement is a worker-lifetime operation on an owned conversation, and it
/// requires quiescence: no foreground slot and no non-terminal task. Work that
/// was queued but never started is cancelled in the same commit, because
/// retirement stops future work. Everything already durable - history,
/// ownership, terminal outcomes and checkpoints - is preserved, and an inherited
/// cutoff in another conversation is unaffected.
fn retire_conversation(
    editor: &mut Editor<'_>,
    conversation_id: ConversationId,
    retired: bool,
) -> Result<(), StateError> {
    // Copy the flags out first: this decides from one consistent view and then
    // writes, rather than holding a read borrow across mutations.
    let (already, owned, busy) = {
        let conversation = editor
            .conversation(conversation_id)
            .ok_or(StateError::UnknownConversation(conversation_id))?;
        (
            conversation.retired,
            conversation.owner_task.is_some(),
            conversation.foreground_turn.is_some(),
        )
    };
    if already == retired {
        return Ok(());
    }
    if !retired {
        editor
            .conversation_mut(conversation_id)
            .expect("validated conversation remains present")
            .retired = false;
        return Ok(());
    }
    if !owned {
        return Err(StateError::ConversationNotOwned(conversation_id));
    }
    let live = busy
        || editor.state().tasks.values().any(|task| {
            task.conversation_id == conversation_id
                && !matches!(task.status, TaskStatus::Terminal(_))
        });
    if live {
        return Err(StateError::ConversationHasLiveWork(conversation_id));
    }

    // The transaction stages these cancellations explicitly so they are durable;
    // applying them here as well keeps the resident invariant true for any caller
    // that applies the mutation without staging them.
    for input_id in queued_inputs(editor.state(), conversation_id) {
        apply_input_disposition(editor, input_id, InputDisposition::Cancelled)?;
    }
    editor
        .conversation_mut(conversation_id)
        .expect("validated conversation remains present")
        .retired = true;
    Ok(())
}

fn apply_input_disposition(
    editor: &mut Editor<'_>,
    input_id: InputId,
    disposition: InputDisposition,
) -> Result<(), StateError> {
    let (target, current) = {
        let input = editor
            .input(input_id)
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
            validate_placement(editor.state(), input_id, target, &current, &placement)?;
        }
        InputDisposition::Queued | InputDisposition::Cancelled => {}
    }

    editor
        .input_mut(input_id)
        .expect("validated input remains present")
        .disposition = disposition;
    editor.set_queued(input_id, disposition == InputDisposition::Queued);
    Ok(())
}

fn reserve_task(
    editor: &mut Editor<'_>,
    task_id: TaskId,
    generation: u64,
    kind: InvocationKind,
) -> Result<(), StateError> {
    let task = editor
        .task(task_id)
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
                    editor.task(*dependency).map(|task| &task.status),
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

    let task = editor
        .task_mut(task_id)
        .expect("validated task remains present");
    task.generation = generation;
    task.invocation = Some(TaskInvocation { generation, kind });
    task.status = TaskStatus::Running;
    Ok(())
}

fn authorized_task_mut<'a>(
    editor: &'a mut Editor<'_>,
    task_id: TaskId,
    generation: u64,
) -> Result<&'a mut TaskRecord, StateError> {
    let task = editor
        .task_mut(task_id)
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
    #[error("inconsistent stored session ({rule}): {detail}")]
    InconsistentReconstruction { rule: &'static str, detail: String },
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

#[cfg(test)]
mod request_page_tests {
    use super::*;
    use crate::conversation::context::ContextControl;
    use crate::{Conversation, Entry, EntryKind, HistoryParent, SessionId};

    /// Root with three entries, a fork of it at the second entry, and one entry
    /// the fork appended itself.
    fn fixture() -> (SessionState, ConversationId, ConversationId, Vec<EntryId>) {
        let mut state = SessionState::empty(SessionId::new());
        let root = ConversationId::new(1).expect("id");
        let fork = ConversationId::new(2).expect("id");
        state.root_conversation = Some(root);
        state.last_seq = Some(LocalSeq::new(9).expect("sequence"));
        state.last_commit = Some(CommitSeq::new(9).expect("commit"));
        state
            .conversations
            .insert(root, Arc::new(Conversation::root(root)));

        let mut ids = Vec::new();
        for (index, conversation) in [root, root, root, fork].into_iter().enumerate() {
            let id = EntryId::new(index as i64 + 1).expect("id");
            ids.push(id);
            state
                .insert_entry(Entry {
                    id,
                    conversation_id: conversation,
                    kind: EntryKind::new("note").expect("kind"),
                    data: serde_json::json!({"index": index}),
                    projection: Vec::new(),
                    context: ContextControl::none(),
                })
                .expect("entry");
        }
        state.conversations.insert(
            fork,
            Arc::new(Conversation {
                id: fork,
                parent: Some(HistoryParent {
                    conversation_id: root,
                    at: ids[1],
                }),
                owner_task: None,
                foreground_turn: None,
                turn_cancelled: false,
                retired: false,
            }),
        );
        (state, root, fork, ids)
    }

    #[test]
    fn a_request_page_never_crosses_its_cut() {
        let (state, root, fork, ids) = fixture();
        let [first, second, third, forked] = [ids[0], ids[1], ids[2], ids[3]];

        // The cut is the last visible entry, and paging stops there.
        assert_eq!(
            state.last_visible_id(root).expect("cut"),
            ContextCut::Through(third)
        );
        let (page, more) = state
            .request_page(root, ContextCut::Through(second), None, 8)
            .expect("page");
        assert_eq!(page, vec![first, second]);
        assert!(!more);

        // A cursor at the cut has nothing after it inside the same request.
        assert_eq!(
            state
                .request_page(root, ContextCut::Through(second), Some(second), 8)
                .expect("page"),
            (Vec::new(), false)
        );

        // A cursor beyond the cut is not part of this view.
        assert!(matches!(
            state.request_page(root, ContextCut::Through(second), Some(third), 8),
            Err(StateError::InvisibleCursor(cursor)) if cursor == third
        ));

        // `Empty` is a boundary of its own, and an unknown cut is refused rather
        // than silently widened to the whole transcript.
        assert_eq!(
            state
                .request_page(root, ContextCut::Empty, None, 8)
                .expect("page"),
            (Vec::new(), false)
        );
        let unknown = EntryId::new(8).expect("id");
        assert!(matches!(
            state.request_page(root, ContextCut::Through(unknown), None, 8),
            Err(StateError::InvisibleCursor(cursor)) if cursor == unknown
        ));

        // The fork sees its inherited prefix plus its own entry, and a cut inside
        // the prefix excludes everything it appended afterwards.
        assert_eq!(
            state.last_visible_id(fork).expect("cut"),
            ContextCut::Through(forked)
        );
        assert_eq!(
            state
                .request_page(fork, ContextCut::Through(second), None, 8)
                .expect("page")
                .0,
            vec![first, second],
            "a cut inside the inherited prefix excludes own entries"
        );
        assert_eq!(
            state
                .request_page(fork, ContextCut::Through(forked), None, 8)
                .expect("page")
                .0,
            vec![first, second, forked]
        );
        assert_eq!(
            state
                .request_page(fork, ContextCut::Through(forked), Some(second), 8)
                .expect("page")
                .0,
            vec![forked]
        );
        assert!(matches!(
            state.request_page(fork, ContextCut::Through(second), Some(forked), 8),
            Err(StateError::InvisibleCursor(cursor)) if cursor == forked
        ));
    }

    #[test]
    fn a_request_page_costs_the_page_not_the_transcript() {
        let (mut state, root, _, _) = fixture();
        for index in 10..200 {
            let id = EntryId::new(index).expect("id");
            state
                .insert_entry(Entry {
                    id,
                    conversation_id: root,
                    kind: EntryKind::new("note").expect("kind"),
                    data: serde_json::json!({"index": index}),
                    projection: Vec::new(),
                    context: ContextControl::none(),
                })
                .expect("entry");
        }
        let cut = state.last_visible_id(root).expect("cut");
        let (page, more) = state.request_page(root, cut, None, 4).expect("page");
        assert_eq!(page.len(), 4);
        assert!(more, "a truncated page reports that more follows");
        let (rest, more) = state
            .request_page(root, cut, Some(page[3]), 4)
            .expect("page");
        assert_eq!(rest.len(), 4);
        assert!(more);
    }
}
