use std::collections::HashSet;

use serde_json::Value;

use crate::conversation::context::ContextControl;
use crate::conversation::context::{project, validate_fork_cutoff};
use crate::session::command::{
    ConversationSpec, EntryRequest, InputRequest, SessionError, TaskRequest,
};
use crate::session::idle::Admission;
use crate::session::idle::admission;
use crate::session::journal::Editor;
use crate::session::state::{SessionState, apply_mutation, queued_inputs};
use crate::task::{PlannedTask, PlannedTurn, TaskDependency, TaskPlan};
use crate::view::Change;
use crate::{
    CommitSeq, Conversation, ConversationConfig, ConversationId, Entry, EntryId, Input,
    InputDisposition, InputId, InputPlacement, InvocationKind, LocalSeq, TaskId, TaskOutcome,
    TaskOutput, TaskRecord, TaskStatus,
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
    /// Cancel the turn holding a conversation's foreground slot.
    ///
    /// Distinct from `MarkTaskCancellation`: a turn outlives its root's
    /// operation, so the barrier is durable turn state rather than another
    /// task's cancellation flag.
    MarkTurnCancelled {
        conversation_id: ConversationId,
        root: TaskId,
    },
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
    /// Install a conversation's complete generation configuration.
    ///
    /// The commit that installed it is derived by the store and by the sealing
    /// path from the batch's own commit sequence, so the payload carries the
    /// content only and the revision cannot disagree with the commit.
    SetConversationConfig {
        conversation_id: ConversationId,
        config: ConversationConfig,
    },
    /// Retire a conversation into a read-only archive, or reactivate it.
    SetConversationRetired {
        conversation_id: ConversationId,
        retired: bool,
    },
    /// Seal a completed turn with the member whose settlement closed it.
    CloseTurn {
        root: TaskId,
        closed_by: TaskId,
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

/// A validated command that has not been persisted yet.
///
/// The write set is what the store applies; the editor is what lets the caller
/// take the prepared writes back if that application fails, so a command that
/// never became durable leaves no trace in resident state.
#[derive(Debug)]
pub(crate) struct Prepared<'a> {
    pub(crate) batch: MutationBatch,
    pub(crate) changes: Vec<Change>,
    editor: Editor<'a>,
}

impl Prepared<'_> {
    /// Undo the prepared writes. The session must not continue serving.
    pub(crate) fn rollback(self) {
        self.editor.rollback();
    }
}

/// One uncommitted command.
///
/// Writes go to resident state through [`Editor`], which can take them back, so
/// preparing a command costs the records it touches instead of a copy of the
/// session. A command that fails validation rolls back; a command whose commit
/// fails fencing the session leaves nothing to roll back.
pub(crate) struct Transaction<'a> {
    base_commit: Option<CommitSeq>,
    editor: Editor<'a>,
    admitted_inputs: Vec<InputId>,
    configured: Vec<ConversationId>,
    writes: Vec<Mutation>,
    changes: Vec<Change>,
    released_turns: Vec<ConversationId>,
}

impl<'a> Transaction<'a> {
    pub(crate) fn new(state: &'a mut SessionState) -> Self {
        Self {
            base_commit: state.last_commit,
            editor: Editor::new(state),
            admitted_inputs: Vec::new(),
            configured: Vec::new(),
            writes: Vec::new(),
            changes: Vec::new(),
            released_turns: Vec::new(),
        }
    }

    pub(crate) fn create_root(&mut self) -> Result<ConversationId, SessionError> {
        if self.editor.root_conversation().is_some() {
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
                .editor
                .state()
                .visible_entries(parent.conversation_id)
                .map_err(map_state)?;
            validate_fork_cutoff(&visible, parent.at)?;
        }
        if let Some(owner_task) = spec.owner_task
            && self.editor.task(owner_task).is_none()
        {
            return Err(SessionError::UnknownTask(owner_task));
        }

        let id = ConversationId::new(self.allocate()?.get())?;
        let conversation = Conversation {
            id,
            parent: spec.parent,
            owner_task: spec.owner_task,
            foreground_turn: None,
            turn_cancelled: false,
            retired: false,
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
        if self.editor.conversation(request.conversation_id).is_none() {
            return Err(SessionError::UnknownConversation(request.conversation_id));
        }
        // A head or edit claims to establish a usable context boundary: it names
        // entries that must be visible here, and the resulting context must be
        // complete. Plain appends claim nothing, so they are validated without
        // reading the transcript at all, and an append does not get slower, or
        // more correct, by materializing the history it appends to.
        let history = if request.context.head.is_some() || !request.context.edits.is_empty() {
            let visible = self
                .editor
                .state()
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
            Some(visible)
        } else {
            None
        };

        let id = EntryId::new(self.allocate()?.get())?;
        let entry = Entry::new(
            id,
            request.conversation_id,
            request.kind,
            request.data,
            request.projection,
            request.context,
        );
        if let Some(mut history) = history {
            history.push(entry.clone());
            project(&history).map_err(SessionError::IncompleteContextControl)?;
        }
        self.stage(Mutation::AppendEntry(entry), Change::EntryAppended(id))?;
        Ok(id)
    }

    /// Apply a trusted task's finalization plan. Conversations this plan creates
    /// come first, then entries, input bindings and successor tasks in plan
    /// order, with all task IDs allocated up front so planned dependencies
    /// resolve without exposing IDs before commit. Any error rolls back the whole
    /// transaction, including the settling task.
    pub(crate) fn apply_task_plan(
        &mut self,
        plan: &TaskPlan,
        settling_task: TaskId,
    ) -> Result<Vec<TaskId>, SessionError> {
        if plan.entries().len() > crate::task::MAX_PLAN_ENTRIES
            || plan.tasks().len() > crate::task::MAX_PLAN_TASKS
            || plan.conversations().len() > crate::task::MAX_PLAN_CONVERSATIONS
        {
            return Err(SessionError::PlanTooLarge {
                entries: plan.entries().len(),
                conversations: plan.conversations().len(),
                tasks: plan.tasks().len(),
            });
        }

        // A conversation this plan creates is owned by the settling task, which
        // records the reciprocal ownership edge in the same commit.
        let mut conversation_ids = Vec::with_capacity(plan.conversations().len());
        for conversation in plan.conversations() {
            conversation_ids.push(self.create_conversation(ConversationSpec {
                parent: conversation.parent,
                owner_task: Some(settling_task),
            })?);
        }

        let mut entry_ids = Vec::with_capacity(plan.entries().len());
        for entry in plan.entries() {
            let conversation_id =
                self.resolve_target(entry.conversation_id, plan, &conversation_ids)?;
            entry_ids.push(self.append_entry(EntryRequest {
                conversation_id,
                kind: entry.kind.clone(),
                data: entry.data.clone(),
                projection: entry.projection.clone(),
                context: entry.context.clone(),
            })?);
        }

        // A successor joins the settling task's turn unless the plan says
        // otherwise: `Own` opens the successor's own conversation slot, and
        // `Background` stays outside cancellation scope.
        let settling_turn = self
            .editor
            .task(settling_task)
            .ok_or(SessionError::UnknownTask(settling_task))?
            .turn;
        // The barrier lives on the turn's conversation, not on the root task, so
        // it still stops cleanup work after the root has settled.
        let turn_cancelled = settling_turn.is_some_and(|root| {
            self.editor.task(root).is_some_and(|task| {
                self.editor
                    .conversation(task.conversation_id)
                    .is_some_and(|conversation| {
                        conversation.foreground_turn == Some(root) && conversation.turn_cancelled
                    })
            })
        });

        let mut planned_ids = Vec::with_capacity(plan.tasks().len());
        for _ in plan.tasks() {
            planned_ids.push(TaskId::new(self.allocate()?.get())?);
        }
        for (index, task) in plan.tasks().iter().enumerate() {
            let conversation_id =
                self.resolve_target(task.conversation_id, plan, &conversation_ids)?;
            let id = planned_ids[index];
            let turn = match task.turn {
                PlannedTurn::Inherit => settling_turn,
                PlannedTurn::Own => Some(id),
                PlannedTurn::Background => None,
            };
            self.create_planned_task(
                task,
                conversation_id,
                id,
                turn,
                turn_cancelled,
                plan.id(),
                &planned_ids,
            )?;
        }
        Ok(planned_ids)
    }

    /// Resolve a plan target to a conversation that exists in this draft.
    fn resolve_target(
        &self,
        target: crate::task::PlannedTarget,
        plan: &TaskPlan,
        conversation_ids: &[ConversationId],
    ) -> Result<ConversationId, SessionError> {
        match target {
            crate::task::PlannedTarget::Existing(conversation_id) => Ok(conversation_id),
            crate::task::PlannedTarget::Planned(reference) => {
                // A plan may only target a conversation planned by the same plan,
                // which rejects a handle minted elsewhere.
                if reference.plan_id() != plan.id() {
                    return Err(SessionError::Invariant(
                        "plan target belongs to a different plan".to_owned(),
                    ));
                }
                conversation_ids
                    .get(reference.index())
                    .copied()
                    .ok_or_else(|| {
                        SessionError::Invariant("plan target was not planned".to_owned())
                    })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn create_planned_task(
        &mut self,
        task: &PlannedTask,
        conversation_id: ConversationId,
        id: TaskId,
        turn: Option<TaskId>,
        turn_cancelled: bool,
        plan_id: u64,
        planned_ids: &[TaskId],
    ) -> Result<(), SessionError> {
        if self.editor.conversation(conversation_id).is_none() {
            return Err(SessionError::UnknownConversation(conversation_id));
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
            if self.editor.task(dependency_id).is_none() {
                return Err(SessionError::UnknownTask(dependency_id));
            }
            dependencies.push(dependency_id);
        }

        let record = TaskRecord::pending(
            id,
            conversation_id,
            task.kind.clone(),
            task.schema_version,
            task.input.clone(),
            dependencies,
        );
        let opens_turn = task.turn == PlannedTurn::Own;
        let mut record = match turn {
            Some(turn) => record.in_turn(turn),
            None => record,
        };
        // A cancelled turn admits no new runnable work. Successors that join the
        // settling turn, or that open their own, are born cancelled so an abort's
        // cleanup cannot smuggle ordinary work into a turn the caller already
        // stopped. `Background` successors are outside turn cancellation by
        // construction, which is the trusted kind's explicit lifetime choice.
        if turn_cancelled && !matches!(task.turn, PlannedTurn::Background) {
            record.cancel_requested = true;
        }
        self.stage(Mutation::CreateTask(record), Change::TaskCreated(id))?;
        if opens_turn {
            // Validated against the same slot invariant as any other turn: one
            // authoritative foreground turn per conversation.
            self.stage(
                Mutation::OpenForegroundTurn {
                    conversation_id,
                    task_id: id,
                },
                Change::ForegroundTurnChanged(conversation_id),
            )?;
        }
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
        // Retirement is enforced by the staging path; this decides only whether
        // the conversation is busy.
        let busy = self
            .editor
            .conversation(target)
            .ok_or(SessionError::UnknownConversation(target))?
            .foreground_turn
            .is_some();

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
                self.place_input(input_id, task_id)?;
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
            .editor
            .input(input_id)
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
        self.place_input(input_id, task_id)?;
        Ok(task_id)
    }

    /// Place an input in its conversation and bind it to the turn that will
    /// answer it, in the commit that creates that turn.
    ///
    /// Placement is independent of answer success: the accepted message becomes
    /// history before any invocation runs, so a cancelled, failed or interrupted
    /// answer cannot strand it outside the transcript. Placement happens here
    /// rather than at admission for a queued input, because a running generation
    /// re-reads the transcript when it freezes a request and must not see a
    /// later turn's message.
    fn place_input(&mut self, input_id: InputId, turn: TaskId) -> Result<EntryId, SessionError> {
        let Some(input) = self.editor.input(input_id) else {
            return Err(SessionError::UnknownInput(input_id));
        };
        if input.disposition != InputDisposition::Queued {
            return Err(SessionError::InvalidInputDisposition(input_id));
        }
        let (target, body) = (input.target, input.body.clone());
        let placement = body.placement();
        let entry = self.append_entry(EntryRequest {
            conversation_id: target,
            kind: placement.kind,
            data: placement.data,
            projection: placement.projection,
            context: ContextControl::none(),
        })?;
        self.set_input_disposition(
            input_id,
            InputDisposition::Placed(InputPlacement { entry, turn }),
        )?;
        Ok(entry)
    }

    /// Start a new answer attempt for an input that is already placed.
    ///
    /// The transcript is untouched: the placed entry is the input's content, and
    /// a retry answers it again instead of admitting the message twice. The
    /// caller names the attempt it is replacing, so a stale retry cannot rebind
    /// an input that has already moved on.
    pub(crate) fn retry_input(
        &mut self,
        input_id: InputId,
        expected_turn: TaskId,
        turn: TaskRequest,
    ) -> Result<TaskId, SessionError> {
        let placement = self.rebindable_placement(input_id, expected_turn)?;
        if turn.conversation_id != self.input_target(input_id)? {
            return Err(SessionError::InputTargetMismatch {
                input: self.input_target(input_id)?,
                task: turn.conversation_id,
            });
        }
        let task_id = self.create_turn(turn)?;
        self.set_input_disposition(
            input_id,
            InputDisposition::Placed(InputPlacement {
                entry: placement.entry,
                turn: task_id,
            }),
        )?;
        Ok(task_id)
    }

    /// Record that a placed input will not be answered by another attempt.
    ///
    /// The placement is preserved: abandonment ends the answer intent, not the
    /// accepted message, and it cancels no work that is already running.
    pub(crate) fn abandon_input(
        &mut self,
        input_id: InputId,
        expected_turn: TaskId,
    ) -> Result<(), SessionError> {
        let placement = self.rebindable_placement(input_id, expected_turn)?;
        self.set_input_disposition(input_id, InputDisposition::Abandoned(placement))
    }

    /// The placement a retry or abandonment may act on.
    ///
    /// The input must be placed, still bound to the attempt the caller names, and
    /// that attempt must have closed its turn: a terminal root can still have live
    /// members holding the slot. What the next attempt would actually send is not
    /// checked here: the binding records answer intent, and what a request
    /// included is evidence in that request, where an edit that drops the placed
    /// content is visible.
    fn rebindable_placement(
        &mut self,
        input_id: InputId,
        expected_turn: TaskId,
    ) -> Result<InputPlacement, SessionError> {
        let (target, placement) = {
            let input = self
                .editor
                .input(input_id)
                .ok_or(SessionError::UnknownInput(input_id))?;
            let Some(placement) = input.disposition.placement() else {
                return Err(SessionError::InputNotPlaced(input_id));
            };
            (input.target, placement)
        };
        if placement.turn != expected_turn {
            return Err(SessionError::StaleInputBinding {
                input: input_id,
                expected: expected_turn,
                current: placement.turn,
            });
        }
        if self
            .editor
            .conversation(target)
            .is_none_or(|conversation| !conversation.accepts_work())
        {
            return Err(SessionError::ConversationRetired(target));
        }
        let closed = self
            .editor
            .task(expected_turn)
            .is_some_and(|task| task.turn_closed_by.is_some());
        if !closed {
            return Err(SessionError::TurnStillOpen(expected_turn));
        }
        Ok(placement)
    }

    fn input_target(&self, input_id: InputId) -> Result<ConversationId, SessionError> {
        self.editor
            .input(input_id)
            .map(|input| input.target)
            .ok_or(SessionError::UnknownInput(input_id))
    }

    /// Install a conversation's complete generation configuration.
    ///
    /// Full replacement with a compare-and-set fence: `expected` is the revision
    /// the caller believes is installed, so a lost update is refused instead of
    /// silently overwriting a configuration committed by someone else. `None`
    /// means "this conversation must still be unconfigured", which is how a
    /// first launch refuses to clobber a configuration it did not see.
    ///
    /// Nothing is partially applied: an invalid configuration is refused before
    /// any write, so the durable record is either the old configuration or the
    /// new one.
    pub(crate) fn configure_conversation(
        &mut self,
        conversation_id: ConversationId,
        expected: Option<CommitSeq>,
        config: ConversationConfig,
    ) -> Result<(), SessionError> {
        let conversation = self
            .editor
            .conversation(conversation_id)
            .ok_or(SessionError::UnknownConversation(conversation_id))?;
        if !conversation.accepts_work() {
            return Err(SessionError::ConversationRetired(conversation_id));
        }
        let current = self.editor.config_revision(conversation_id);
        if current != expected {
            return Err(SessionError::StaleConfiguration {
                conversation: conversation_id,
                expected,
                current,
            });
        }
        config
            .validate()
            .map_err(|error| SessionError::InvalidConfiguration(error.to_string()))?;
        self.stage(
            Mutation::SetConversationConfig {
                conversation_id,
                config,
            },
            Change::ConversationConfigured(conversation_id),
        )?;
        self.configured.push(conversation_id);
        Ok(())
    }

    /// Retire a conversation into a read-only archive. See
    /// `retire_conversation` in `state.rs` for the preconditions.
    pub(crate) fn retire_conversation(
        &mut self,
        conversation_id: ConversationId,
    ) -> Result<(), SessionError> {
        // Queued input is cancelled in the same commit as the flag: retirement
        // stops future work, so the cancellation has to be durable rather than a
        // resident side effect that a reopen would undo.
        for input_id in queued_inputs(self.editor.state(), conversation_id) {
            self.set_input_disposition(input_id, InputDisposition::Cancelled)?;
        }
        self.stage(
            Mutation::SetConversationRetired {
                conversation_id,
                retired: true,
            },
            Change::ConversationRetirementChanged(conversation_id),
        )
    }

    /// Reactivate a retired conversation. This clears the flag and nothing else:
    /// no task is created, no input is resurrected and no drive is scheduled.
    pub(crate) fn reactivate_conversation(
        &mut self,
        conversation_id: ConversationId,
    ) -> Result<(), SessionError> {
        self.stage(
            Mutation::SetConversationRetired {
                conversation_id,
                retired: false,
            },
            Change::ConversationRetirementChanged(conversation_id),
        )
    }

    pub(crate) fn queue_input(&mut self, request: InputRequest) -> Result<InputId, SessionError> {
        if self.editor.conversation(request.target).is_none() {
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
            .editor
            .conversation(conversation_id)
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
        if self.editor.conversation(request.conversation_id).is_none() {
            return Err(SessionError::UnknownConversation(request.conversation_id));
        }
        let mut dependencies = HashSet::with_capacity(request.dependencies.len());
        for dependency in &request.dependencies {
            if !dependencies.insert(*dependency) {
                return Err(SessionError::DuplicateDependency(*dependency));
            }
            if self.editor.task(*dependency).is_none() {
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

    /// Cancel a foreground turn: mark the turn itself, then every non-terminal
    /// task scoped to it.
    ///
    /// Terminal tasks are left untouched so settled work is never rewritten, and
    /// the turn barrier is what stops a still-live member's cleanup from
    /// creating runnable successors after its root has settled. Owned
    /// conversations and background tasks are deliberately outside this scope.
    pub(crate) fn cancel_turn(&mut self, root: TaskId) -> Result<Vec<TaskId>, SessionError> {
        let root_task = self
            .editor
            .task(root)
            .ok_or(SessionError::UnknownTask(root))?;
        let conversation_id = root_task.conversation_id;
        let holds_slot = self
            .editor
            .conversation(conversation_id)
            .is_some_and(|conversation| conversation.foreground_turn == Some(root));
        if holds_slot {
            self.stage(
                Mutation::MarkTurnCancelled {
                    conversation_id,
                    root,
                },
                Change::ForegroundTurnChanged(conversation_id),
            )?;
        }
        let mut cancelled = Vec::new();
        for task_id in self.editor.state().tasks_of_turn(root) {
            let Some(task) = self.editor.task(task_id) else {
                continue;
            };
            if matches!(task.status, TaskStatus::Terminal(_)) || task.cancel_requested {
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
            .editor
            .task(task_id)
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
            .editor
            .task(task_id)
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
        let turn = self.editor.task(task_id).and_then(|task| task.turn);
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
            .editor
            .state()
            .conversations
            .values()
            .find(|conversation| conversation.foreground_turn == Some(root))
            .map(|conversation| conversation.id);
        let Some(conversation_id) = holder else {
            return Ok(());
        };
        // Only this turn's members can keep the slot held, so ask the turn
        // index rather than every task in the session.
        let remaining = self
            .editor
            .state()
            .tasks_of_turn(root)
            .iter()
            .any(|task_id| {
                self.editor
                    .task(*task_id)
                    .is_some_and(|task| !matches!(task.status, TaskStatus::Terminal(_)))
            });
        if !remaining {
            // The turn's completion receipt commits with the settlement that
            // closed it, so a joiner can never observe a released slot without
            // knowing which member ended the turn.
            self.stage(
                Mutation::CloseTurn {
                    root,
                    closed_by: task_id,
                },
                Change::TurnClosed(root),
            )?;
            self.stage(
                Mutation::ReleaseForegroundTurn {
                    conversation_id,
                    task_id: root,
                },
                Change::ForegroundTurnChanged(conversation_id),
            )?;
            self.released_turns.push(conversation_id);
        }
        Ok(())
    }

    /// Conversations whose foreground slot this transaction releases. A
    /// settlement uses this to schedule only the conversation that actually
    /// became idle, rather than every conversation it touched.
    pub(crate) fn take_released_turns(&mut self) -> Vec<ConversationId> {
        std::mem::take(&mut self.released_turns)
    }

    /// Seal the command into a durable write set.
    ///
    /// A sealing failure rolls the prepared writes back, so the caller sees a
    /// session that is exactly as it was. On success the caller owns a
    /// [`Prepared`], which can still take the writes back if persistence fails:
    /// resident state must not keep a command that never became durable.
    pub(crate) fn finish(mut self) -> Result<Prepared<'a>, SessionError> {
        let last_seq = match self.allocate() {
            Ok(seq) => seq,
            Err(error) => {
                self.editor.rollback();
                return Err(error);
            }
        };
        let commit_seq = match CommitSeq::new(last_seq.get()) {
            Ok(commit_seq) => commit_seq,
            Err(error) => {
                self.editor.rollback();
                return Err(error.into());
            }
        };
        let admitted = std::mem::take(&mut self.admitted_inputs);
        for input_id in admitted {
            self.editor.set_input_commit(input_id, commit_seq);
        }
        // A configuration's revision is the commit that installed it, which only
        // exists once the sequence is allocated. Binding it here keeps the
        // compare-and-set basis exactly the value the caller was handed.
        let configured = std::mem::take(&mut self.configured);
        for conversation_id in configured {
            self.editor.set_config_revision(conversation_id, commit_seq);
        }
        self.editor.set_last_commit(commit_seq);
        Ok(Prepared {
            batch: MutationBatch {
                commit_seq,
                last_seq,
                base_commit: self.base_commit,
                writes: std::mem::take(&mut self.writes),
            },
            changes: std::mem::take(&mut self.changes),
            editor: self.editor,
        })
    }

    /// Take back every write this command made.
    pub(crate) fn rollback(self) {
        self.editor.rollback();
    }

    fn allocate(&mut self) -> Result<LocalSeq, SessionError> {
        let next = match self.editor.last_seq() {
            Some(current) => current.next()?,
            None => LocalSeq::new(1)?,
        };
        self.editor.set_last_seq(next);
        Ok(next)
    }

    fn stage(&mut self, mutation: Mutation, change: Change) -> Result<(), SessionError> {
        apply_mutation(&mut self.editor, &mutation).map_err(map_state)?;
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
        crate::session::state::StateError::ConversationRetired(id) => {
            SessionError::ConversationRetired(id)
        }
        crate::session::state::StateError::ConversationNotOwned(id) => {
            SessionError::ConversationNotOwned(id)
        }
        crate::session::state::StateError::ConversationHasLiveWork(id) => {
            SessionError::ConversationHasLiveWork(id)
        }
        other => SessionError::Invariant(other.to_string()),
    }
}
