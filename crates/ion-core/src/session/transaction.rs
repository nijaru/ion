use std::collections::HashSet;

use serde_json::Value;

use crate::conversation::context::{project, validate_fork_cutoff};
use crate::session::command::{
    ConversationSpec, EntryRequest, InputRequest, SessionError, TaskRequest,
};
use crate::session::idle::Admission;
use crate::session::idle::admission;
use crate::session::state::{SessionState, apply_mutation};
use crate::task::{PlannedTask, TaskDependency, TaskPlan};
use crate::view::Change;
use crate::{
    CommitSeq, Conversation, ConversationId, Entry, EntryId, Input, InputDisposition, InputId,
    InvocationKind, LocalSeq, TaskId, TaskOutcome, TaskOutput, TaskRecord, TaskStatus,
};

#[derive(Debug, Clone)]
pub(crate) enum Mutation {
    CreateRoot(Conversation),
    CreateConversation(Conversation),
    AppendEntry(Entry),
    AdmitInput(Input),
    SetInputDisposition {
        input_id: InputId,
        disposition: InputDisposition,
    },
    CreateTask(TaskRecord),
    OpenForegroundTurn {
        conversation_id: ConversationId,
        task_id: TaskId,
    },
    ReserveTask {
        task_id: TaskId,
        generation: u64,
        kind: InvocationKind,
    },
    CheckpointTask {
        task_id: TaskId,
        generation: u64,
        checkpoint: Option<Value>,
        output: Option<TaskOutput>,
    },
    MarkTaskCancellation(TaskId),
    SettleTask {
        task_id: TaskId,
        generation: u64,
        outcome: TaskOutcome,
        output: Option<TaskOutput>,
    },
    AttachOwnedConversation {
        task_id: TaskId,
        conversation_id: ConversationId,
    },
    /// Release a conversation's foreground slot once its turn has no remaining
    /// non-terminal members.
    ReleaseForegroundTurn {
        conversation_id: ConversationId,
        task_id: TaskId,
    },
}

/// The complete durable write set of one commit. Observation invalidations are
/// kept separately by the caller so persistence never depends on the view
/// vocabulary. A store must be able to reconstruct equivalent semantic records
/// from `writes` alone.
#[derive(Debug, Clone)]
pub(crate) struct MutationBatch {
    pub(crate) commit_seq: CommitSeq,
    pub(crate) last_seq: LocalSeq,
    /// The commit this batch was built against. A real store uses it as a
    /// compare-and-set fence so a stale writer authority cannot interleave.
    pub(crate) base_commit: Option<CommitSeq>,
    pub(crate) writes: Vec<Mutation>,
}

pub(crate) struct Transaction {
    base_commit: Option<CommitSeq>,
    draft: SessionState,
    admitted_inputs: Vec<InputId>,
    writes: Vec<Mutation>,
    changes: Vec<Change>,
}

impl Transaction {
    pub(crate) fn new(state: &SessionState) -> Self {
        Self {
            base_commit: state.last_commit,
            draft: state.clone(),
            admitted_inputs: Vec::new(),
            writes: Vec::new(),
            changes: Vec::new(),
        }
    }

    pub(crate) fn create_root(&mut self) -> Result<ConversationId, SessionError> {
        if self.draft.root_conversation.is_some() {
            return Err(SessionError::Invariant(
                "root conversation already exists".to_owned(),
            ));
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
        if let Some(owner_task) = spec.owner_task
            && !self.draft.tasks.contains_key(&owner_task)
        {
            return Err(SessionError::UnknownTask(owner_task));
        }

        let id = ConversationId::new(self.allocate()?.get())?;
        let conversation = Conversation {
            id,
            parent: spec.parent,
            owner_task: spec.owner_task,
            foreground_turn: None,
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
        if !self
            .draft
            .conversations
            .contains_key(&request.conversation_id)
        {
            return Err(SessionError::UnknownConversation(request.conversation_id));
        }
        let visible = self
            .draft
            .visible_entries(request.conversation_id)
            .map_err(map_state)?;
        let visible_ids: HashSet<_> = visible.iter().map(|entry| entry.id).collect();
        if let Some(head) = request.context.head
            && !visible_ids.contains(&head)
        {
            return Err(SessionError::InvisibleContextReference(head));
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
        // A head or edit claims to establish a usable context boundary, so the
        // resulting provider-neutral context must be complete now. Plain appends
        // remain unvalidated so in-flight tool exchanges are still durable.
        if entry.context.head.is_some() || !entry.context.edits.is_empty() {
            let mut history = self
                .draft
                .visible_entries(request.conversation_id)
                .map_err(map_state)?;
            history.push(entry.clone());
            project(&history).map_err(SessionError::IncompleteContextControl)?;
        }
        self.stage(Mutation::AppendEntry(entry), Change::EntryAppended(id))?;
        Ok(id)
    }

    /// Apply a trusted task's finalization plan. Entries are staged first, then
    /// successor tasks in plan order with all IDs allocated up front so planned
    /// dependencies resolve without exposing IDs before commit. Any error rolls
    /// back the whole transaction, including the settling task.
    pub(crate) fn apply_task_plan(
        &mut self,
        plan: &TaskPlan,
        settling_task: TaskId,
    ) -> Result<Vec<TaskId>, SessionError> {
        if plan.entries().len() > crate::task::MAX_PLAN_ENTRIES
            || plan.tasks().len() > crate::task::MAX_PLAN_TASKS
            || plan.inputs().len() > crate::task::MAX_PLAN_INPUTS
        {
            return Err(SessionError::PlanTooLarge {
                entries: plan.entries().len(),
                inputs: plan.inputs().len(),
                tasks: plan.tasks().len(),
            });
        }
        let mut entry_ids = Vec::with_capacity(plan.entries().len());
        for entry in plan.entries() {
            entry_ids.push(self.append_entry(EntryRequest {
                conversation_id: entry.conversation_id,
                kind: entry.kind.clone(),
                data: entry.data.clone(),
                projection: entry.projection.clone(),
                context: entry.context.clone(),
            })?);
        }

        // An input and the transcript entry that carries it become durable in the
        // same commit, so a consumed input always has its content present.
        for binding in plan.inputs() {
            if binding.entry.plan_id() != plan.id() {
                return Err(SessionError::Invariant(
                    "plan consumes an entry from a different plan".to_owned(),
                ));
            }
            let entry_id = *entry_ids.get(binding.entry.index()).ok_or_else(|| {
                SessionError::Invariant("plan consumes an entry that was not planned".to_owned())
            })?;
            self.set_input_disposition(binding.input, InputDisposition::Consumed(entry_id))?;
        }

        // Successors inherit the settling task's turn, unless the plan marks
        // them background (a retained worker must survive turn cancellation).
        let inherited_turn = self
            .draft
            .tasks
            .get(&settling_task)
            .ok_or(SessionError::UnknownTask(settling_task))?
            .turn;

        let mut planned_ids = Vec::with_capacity(plan.tasks().len());
        for _ in plan.tasks() {
            planned_ids.push(TaskId::new(self.allocate()?.get())?);
        }
        for (index, task) in plan.tasks().iter().enumerate() {
            let turn = if task.background {
                None
            } else {
                inherited_turn
            };
            self.create_planned_task(task, planned_ids[index], turn, plan.id(), &planned_ids)?;
        }
        Ok(planned_ids)
    }

    fn create_planned_task(
        &mut self,
        task: &PlannedTask,
        id: TaskId,
        turn: Option<TaskId>,
        plan_id: u64,
        planned_ids: &[TaskId],
    ) -> Result<(), SessionError> {
        if !self.draft.conversations.contains_key(&task.conversation_id) {
            return Err(SessionError::UnknownConversation(task.conversation_id));
        }
        let mut seen = HashSet::with_capacity(task.dependencies.len());
        let mut dependencies = Vec::with_capacity(task.dependencies.len());
        for dependency in &task.dependencies {
            let dependency_id = match dependency {
                TaskDependency::Existing(id) => *id,
                TaskDependency::Planned(reference) => {
                    // A plan may only depend on tasks planned earlier in the same
                    // plan, which keeps the successor graph acyclic and rejects a
                    // handle minted by a different plan.
                    if reference.plan_id() != plan_id {
                        return Err(SessionError::Invariant(
                            "plan dependency belongs to a different plan".to_owned(),
                        ));
                    }
                    *planned_ids.get(reference.index()).ok_or_else(|| {
                        SessionError::Invariant("plan dependency is not yet planned".to_owned())
                    })?
                }
            };
            if !seen.insert(dependency_id) {
                return Err(SessionError::DuplicateDependency(dependency_id));
            }
            if !self.draft.tasks.contains_key(&dependency_id) {
                return Err(SessionError::UnknownTask(dependency_id));
            }
            dependencies.push(dependency_id);
        }

        let record = TaskRecord::pending(
            id,
            task.conversation_id,
            task.kind.clone(),
            task.schema_version,
            task.input.clone(),
            dependencies,
        );
        let mut record = match turn {
            Some(turn) => record.in_turn(turn),
            None => record,
        };
        // A cancelled turn admits no new runnable work. Successors that inherit
        // the turn are born cancelled so an abort's cleanup cannot smuggle
        // ordinary work into a turn the caller already stopped. `background`
        // successors are outside the turn by construction and are the trusted
        // kind's explicit lifetime choice.
        if record.turn.is_some_and(|root| {
            self.draft
                .tasks
                .get(&root)
                .is_some_and(|task| task.cancel_requested)
        }) {
            record.cancel_requested = true;
        }
        self.stage(Mutation::CreateTask(record), Change::TaskCreated(id))?;
        Ok(())
    }

    /// Admit an input and apply the mode/state policy for its conversation.
    ///
    /// `turn` supplies the turn to start when the policy starts one; a mode that
    /// only queues does not need it. Refusal happens before anything is staged,
    /// and any later error rolls the whole transaction back, so a rejected
    /// submission admits nothing.
    pub(crate) fn admit_input(
        &mut self,
        request: InputRequest,
        turn: Option<TaskRequest>,
    ) -> Result<(InputId, Option<TaskId>), SessionError> {
        let target = request.target;
        let mode = request.mode;
        let conversation = self
            .draft
            .conversations
            .get(&target)
            .ok_or(SessionError::UnknownConversation(target))?;
        let busy = conversation.foreground_turn.is_some();

        match admission(mode, busy) {
            Admission::Reject => Err(SessionError::ForegroundTurnBusy(target)),
            Admission::Queue => Ok((self.queue_input(request)?, None)),
            Admission::StartTurn => {
                let turn = turn.ok_or(SessionError::MissingTurnRequest { mode })?;
                if turn.conversation_id != target {
                    return Err(SessionError::InputTargetMismatch {
                        input: target,
                        task: turn.conversation_id,
                    });
                }
                let input_id = self.queue_input(request)?;
                let task_id = self.create_turn(turn)?;
                self.set_input_disposition(input_id, InputDisposition::Assigned(task_id))?;
                Ok((input_id, Some(task_id)))
            }
        }
    }

    /// Bind an already-queued input to a new turn in one commit. This is how an
    /// idle conversation answers input that arrived while it was busy.
    pub(crate) fn bind_turn_for_input(
        &mut self,
        input_id: InputId,
        turn: TaskRequest,
    ) -> Result<TaskId, SessionError> {
        let (target, disposition) = self
            .draft
            .inputs
            .get(&input_id)
            .map(|input| (input.target, input.disposition))
            .ok_or(SessionError::UnknownInput(input_id))?;
        if disposition != InputDisposition::Queued {
            return Err(SessionError::InvalidInputDisposition(input_id));
        }
        if target != turn.conversation_id {
            return Err(SessionError::InputTargetMismatch {
                input: target,
                task: turn.conversation_id,
            });
        }
        let task_id = self.create_turn(turn)?;
        self.set_input_disposition(input_id, InputDisposition::Assigned(task_id))?;
        Ok(task_id)
    }

    pub(crate) fn queue_input(&mut self, request: InputRequest) -> Result<InputId, SessionError> {
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
        self.admitted_inputs.push(id);
        Ok(id)
    }

    pub(crate) fn set_input_disposition(
        &mut self,
        input_id: InputId,
        disposition: InputDisposition,
    ) -> Result<(), SessionError> {
        self.stage(
            Mutation::SetInputDisposition {
                input_id,
                disposition,
            },
            Change::InputDispositionChanged(input_id),
        )
    }

    pub(crate) fn create_task(&mut self, request: TaskRequest) -> Result<TaskId, SessionError> {
        self.create_task_record(request, false)
    }

    /// Create a task that becomes the root of a new foreground turn. The
    /// conversation holds one authoritative foreground slot at a time.
    pub(crate) fn create_turn(&mut self, request: TaskRequest) -> Result<TaskId, SessionError> {
        let conversation_id = request.conversation_id;
        if self
            .draft
            .conversations
            .get(&conversation_id)
            .and_then(|conversation| conversation.foreground_turn)
            .is_some()
        {
            return Err(SessionError::ForegroundTurnBusy(conversation_id));
        }
        let id = self.create_task_record(request, true)?;
        self.stage(
            Mutation::OpenForegroundTurn {
                conversation_id,
                task_id: id,
            },
            Change::ForegroundTurnChanged(conversation_id),
        )?;
        Ok(id)
    }

    fn create_task_record(
        &mut self,
        request: TaskRequest,
        foreground: bool,
    ) -> Result<TaskId, SessionError> {
        if !self
            .draft
            .conversations
            .contains_key(&request.conversation_id)
        {
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
        let mut task = TaskRecord::pending(
            id,
            request.conversation_id,
            request.kind,
            request.schema_version,
            request.input,
            request.dependencies,
        );
        if foreground {
            task = task.in_turn(id);
        }
        self.stage(Mutation::CreateTask(task), Change::TaskCreated(id))?;
        Ok(id)
    }

    /// Cancel a foreground turn: every non-terminal task scoped to the turn
    /// root, including the root itself. Terminal tasks are left untouched so
    /// settled work is never rewritten. Owned conversations and background
    /// tasks are deliberately outside this scope.
    pub(crate) fn cancel_turn(&mut self, root: TaskId) -> Result<Vec<TaskId>, SessionError> {
        if !self.draft.tasks.contains_key(&root) {
            return Err(SessionError::UnknownTask(root));
        }
        let mut cancelled = Vec::new();
        for task in self.draft.tasks.values() {
            if task.turn != Some(root)
                || matches!(task.status, TaskStatus::Terminal(_))
                || task.cancel_requested
            {
                continue;
            }
            cancelled.push(task.id);
        }
        for task_id in &cancelled {
            self.stage(
                Mutation::MarkTaskCancellation(*task_id),
                Change::TaskCancellationMarked(*task_id),
            )?;
        }
        Ok(cancelled)
    }

    pub(crate) fn reserve_task(
        &mut self,
        task_id: TaskId,
        kind: InvocationKind,
    ) -> Result<u64, SessionError> {
        let task = self
            .draft
            .tasks
            .get(&task_id)
            .ok_or(SessionError::UnknownTask(task_id))?;
        let generation = task
            .generation
            .checked_add(1)
            .ok_or(SessionError::GenerationExhausted(task_id))?;
        self.stage(
            Mutation::ReserveTask {
                task_id,
                generation,
                kind,
            },
            Change::TaskReserved {
                task_id,
                generation,
                kind,
            },
        )?;
        Ok(generation)
    }

    pub(crate) fn checkpoint_task(
        &mut self,
        task_id: TaskId,
        generation: u64,
        checkpoint: Option<Value>,
        output: Option<TaskOutput>,
    ) -> Result<(), SessionError> {
        self.assert_task_write_authority(task_id, generation)?;
        self.stage(
            Mutation::CheckpointTask {
                task_id,
                generation,
                checkpoint,
                output,
            },
            Change::TaskCheckpointed {
                task_id,
                generation,
            },
        )
    }

    pub(crate) fn mark_task_cancellation(&mut self, task_id: TaskId) -> Result<(), SessionError> {
        self.stage(
            Mutation::MarkTaskCancellation(task_id),
            Change::TaskCancellationMarked(task_id),
        )
    }

    pub(crate) fn assert_task_write_authority(
        &self,
        task_id: TaskId,
        generation: u64,
    ) -> Result<(), SessionError> {
        let task = self
            .draft
            .tasks
            .get(&task_id)
            .ok_or(SessionError::UnknownTask(task_id))?;
        authorize_task_write(task, generation)
    }

    pub(crate) fn settle_task(
        &mut self,
        task_id: TaskId,
        generation: u64,
        outcome: TaskOutcome,
        output: Option<TaskOutput>,
    ) -> Result<(), SessionError> {
        self.assert_task_write_authority(task_id, generation)?;
        let turn = self.draft.tasks.get(&task_id).and_then(|task| task.turn);
        self.stage(
            Mutation::SettleTask {
                task_id,
                generation,
                outcome,
                output,
            },
            Change::TaskSettled(task_id),
        )?;

        // The foreground slot covers the whole chain, not just the root task.
        // Release it only when no non-terminal member of the turn remains.
        let Some(root) = turn else {
            return Ok(());
        };
        let holder = self
            .draft
            .conversations
            .values()
            .find(|conversation| conversation.foreground_turn == Some(root))
            .map(|conversation| conversation.id);
        let Some(conversation_id) = holder else {
            return Ok(());
        };
        let remaining =
            self.draft.tasks.values().any(|task| {
                task.turn == Some(root) && !matches!(task.status, TaskStatus::Terminal(_))
            });
        if !remaining {
            self.stage(
                Mutation::ReleaseForegroundTurn {
                    conversation_id,
                    task_id: root,
                },
                Change::ForegroundTurnChanged(conversation_id),
            )?;
        }
        Ok(())
    }

    pub(crate) fn finish(
        mut self,
    ) -> Result<(MutationBatch, Vec<Change>, SessionState), SessionError> {
        let last_seq = self.allocate()?;
        let commit_seq = CommitSeq::new(last_seq.get())?;
        for input_id in &self.admitted_inputs {
            self.draft.input_commits.insert(*input_id, commit_seq);
        }
        self.draft.last_commit = Some(commit_seq);
        Ok((
            MutationBatch {
                commit_seq,
                last_seq,
                base_commit: self.base_commit,
                writes: self.writes,
            },
            self.changes,
            self.draft,
        ))
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
        self.writes.push(mutation);
        self.changes.push(change);
        Ok(())
    }
}

fn authorize_task_write(task: &TaskRecord, generation: u64) -> Result<(), SessionError> {
    if !matches!(task.status, TaskStatus::Running) {
        return Err(SessionError::TaskNotRunning(task.id));
    }
    if task.generation != generation {
        return Err(SessionError::StaleInvocation {
            task_id: task.id,
            generation,
            current: task.generation,
        });
    }
    let invocation = task.invocation.ok_or_else(|| {
        SessionError::Invariant("running task has no active invocation".to_owned())
    })?;
    if task.cancel_requested && invocation.kind != InvocationKind::Abort {
        return Err(SessionError::CancellationFence(task.id));
    }
    Ok(())
}

fn map_state(error: crate::session::state::StateError) -> SessionError {
    match error {
        crate::session::state::StateError::UnknownConversation(id) => {
            SessionError::UnknownConversation(id)
        }
        crate::session::state::StateError::UnknownInput(id) => SessionError::UnknownInput(id),
        crate::session::state::StateError::UnknownTask(id) => SessionError::UnknownTask(id),
        crate::session::state::StateError::TaskNotPending(id) => SessionError::TaskNotPending(id),
        crate::session::state::StateError::TaskNotRunning(id) => SessionError::TaskNotRunning(id),
        crate::session::state::StateError::TaskAlreadyTerminal(id) => {
            SessionError::TaskAlreadyTerminal(id)
        }
        crate::session::state::StateError::DependenciesNotReady(id) => {
            SessionError::DependenciesNotReady(id)
        }
        crate::session::state::StateError::InvalidInvocationKind { task_id, kind } => {
            SessionError::InvalidInvocationKind { task_id, kind }
        }
        crate::session::state::StateError::StaleInvocation {
            task_id,
            generation,
            current,
        } => SessionError::StaleInvocation {
            task_id,
            generation,
            current,
        },
        crate::session::state::StateError::CancellationFence(id) => {
            SessionError::CancellationFence(id)
        }
        crate::session::state::StateError::InvalidInputDisposition(id) => {
            SessionError::InvalidInputDisposition(id)
        }
        crate::session::state::StateError::ForegroundTurnBusy(id) => {
            SessionError::ForegroundTurnBusy(id)
        }
        other => SessionError::Invariant(other.to_string()),
    }
}
