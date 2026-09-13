use std::collections::VecDeque;

use serde_json::Value;

use crate::session::command::{
    CancellationReceipt, ConversationReceipt, ConversationSpec, EntryReceipt, EntryRequest,
    InputReceipt, InputRequest, InvocationReceipt, SessionError, SubmissionReceipt, TaskReceipt,
    TaskRequest, TurnCancellation,
};
use crate::session::state::{RunnableTask, SessionState};
use crate::session::transaction::Transaction;
use crate::store::{MemoryStore, Persistence};
use crate::view::{
    CommitEvent, EntryPage, ObservationBatch, SessionSnapshot, SessionSummary, TaskCounts,
};
use crate::{
    CommitSeq, ConversationId, DependencyOutcome, InputDisposition, InputId, InvocationKind,
    RequestKey, SessionId, TaskId, TaskOutcome, TaskOutput, TaskRecord, TaskStatus,
};

#[cfg(test)]
#[path = "persistence_tests.rs"]
mod persistence_tests;

const OBSERVATION_CAPACITY: usize = 128;

#[derive(Debug)]
pub struct Session {
    state: SessionState,
    store: Box<dyn Persistence>,
    closed: bool,
    fault: tokio_util::sync::CancellationToken,
    observations: VecDeque<CommitEvent>,
    dropped_through: Option<CommitSeq>,
    changes: tokio::sync::watch::Sender<()>,
}

impl Session {
    pub fn new() -> Result<Self, SessionError> {
        Self::with_id(SessionId::new())
    }

    /// Create a new on-disk session at `path`.
    ///
    /// The file gets a fresh schema; an existing database is refused rather
    /// than overwritten. Commits from this point survive process death.
    pub fn create(path: impl AsRef<std::path::Path>) -> Result<Self, SessionError> {
        let session_id = SessionId::new();
        let store = crate::store::sqlite::SqliteStore::create(path.as_ref(), session_id)
            .map_err(|error| SessionError::Persistence(error.to_string()))?;
        Self::with_store(session_id, Box::new(store))
    }

    /// Open an existing on-disk session.
    ///
    /// This reads durable records only. A task that was running when the
    /// process died stays `Running` and requires an explicit recovery drive, so
    /// opening a session never starts work.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, SessionError> {
        let (store, state) = crate::store::sqlite::SqliteStore::open(path.as_ref())
            .map_err(|error| SessionError::Persistence(error.to_string()))?;
        Ok(Self::from_state(state, Box::new(store)))
    }

    pub fn with_id(session_id: SessionId) -> Result<Self, SessionError> {
        Self::with_store(session_id, Box::new(MemoryStore::new()))
    }

    /// Build a session over an explicit persistence sink. Tests use it to
    /// verify that a committed write set reconstructs resident semantics; K4
    /// reopen uses the same seam to install a reconstructed store.
    pub(crate) fn with_store(
        session_id: SessionId,
        store: Box<dyn Persistence>,
    ) -> Result<Self, SessionError> {
        let mut session = Self::from_state(SessionState::empty(session_id), store);
        session.transact(|transaction| transaction.create_root())?;
        Ok(session)
    }

    /// Build a session over already-reconstructed resident state.
    fn from_state(state: SessionState, store: Box<dyn Persistence>) -> Self {
        Self {
            state,
            store,
            closed: false,
            fault: tokio_util::sync::CancellationToken::new(),
            observations: VecDeque::new(),
            dropped_through: None,
            changes: tokio::sync::watch::channel(()).0,
        }
    }

    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.state.session_id
    }

    #[must_use]
    pub fn root_conversation(&self) -> ConversationId {
        self.state
            .root_conversation
            .expect("initialized session has root conversation")
    }

    pub fn create_conversation(
        &mut self,
        spec: ConversationSpec,
    ) -> Result<ConversationReceipt, SessionError> {
        let (conversation_id, commit_seq) =
            self.transact(|transaction| transaction.create_conversation(spec))?;
        Ok(ConversationReceipt {
            conversation_id,
            commit_seq,
        })
    }

    pub fn append_entry(&mut self, request: EntryRequest) -> Result<EntryReceipt, SessionError> {
        let (entry_id, commit_seq) =
            self.transact(|transaction| transaction.append_entry(request))?;
        Ok(EntryReceipt {
            entry_id,
            commit_seq,
        })
    }

    pub fn create_task(&mut self, request: TaskRequest) -> Result<TaskReceipt, SessionError> {
        let (task_id, commit_seq) =
            self.transact(|transaction| transaction.create_task(request))?;
        Ok(TaskReceipt {
            task_id,
            commit_seq,
        })
    }

    /// Start a new foreground turn on `request.conversation_id`. The created
    /// task is the turn root; successors created by its finalization plan
    /// inherit the turn. Rejects if the conversation already has a live turn.
    pub fn create_turn(&mut self, request: TaskRequest) -> Result<TaskReceipt, SessionError> {
        let (task_id, commit_seq) =
            self.transact(|transaction| transaction.create_turn(request))?;
        Ok(TaskReceipt {
            task_id,
            commit_seq,
        })
    }

    /// Cancel every non-terminal task scoped to the foreground turn rooted at
    /// `root`. Returns the affected task ids so the driver can signal local
    /// invocations. Durable cancellation is committed before any signal.
    pub(crate) fn cancel_turn(&mut self, root: TaskId) -> Result<TurnCancellation, SessionError> {
        self.ensure_open()?;
        let (cancelled, commit_seq) = self.transact(|transaction| transaction.cancel_turn(root))?;
        Ok(TurnCancellation {
            commit_seq,
            cancelled,
        })
    }

    pub fn admit_input(&mut self, request: InputRequest) -> Result<InputReceipt, SessionError> {
        self.ensure_open()?;
        if let Some(key) = request.request_key.as_ref()
            && let Some(receipt) = self.replay_input(key, &request)?
        {
            return Ok(receipt);
        }

        let (input_id, commit_seq) =
            self.transact(|transaction| transaction.admit_input(request))?;
        Ok(InputReceipt {
            input_id,
            commit_seq,
            replayed: false,
        })
    }

    /// Admit `input` and open the foreground turn that answers it, in one
    /// commit. The input is bound to the created turn root.
    ///
    /// A duplicate request key replays the original submission without a new
    /// commit and without opening a second turn; the same key with different
    /// content or routing is a conflict. Nothing is driven here: admitting work
    /// never starts it.
    pub fn submit_input(
        &mut self,
        input: InputRequest,
        task: TaskRequest,
    ) -> Result<SubmissionReceipt, SessionError> {
        self.ensure_open()?;
        if input.target != task.conversation_id {
            return Err(SessionError::InputTargetMismatch {
                input: input.target,
                task: task.conversation_id,
            });
        }
        if let Some(key) = input.request_key.as_ref()
            && let Some(receipt) = self.replay_submission(key, &input)?
        {
            return Ok(receipt);
        }

        let ((input_id, task_id), commit_seq) =
            self.transact(|transaction| transaction.create_input_turn(input, task))?;
        Ok(SubmissionReceipt {
            input_id,
            task_id: Some(task_id),
            commit_seq,
            replayed: false,
        })
    }

    /// Whether a task wait should resolve: terminal, cancelled, or fully
    /// unblocked. Kept on the state owner so the wait and dispatch paths cannot
    /// drift apart.
    pub(crate) fn ready_to_run(&self, task: &TaskRecord) -> bool {
        self.state.ready(task)
    }

    /// Pending work that `settled` just made runnable. Readiness only; the
    /// driver decides whether a candidate has an implementation to run.
    pub(crate) fn runnable_successors(
        &self,
        settled: TaskId,
        created: &[TaskId],
    ) -> Vec<RunnableTask> {
        self.state.runnable_successors(settled, created)
    }

    /// The committed outcomes of a task's fixed dependencies, in dependency
    /// order. A dependency that is not terminal is a broken invariant rather
    /// than an absence: reservation already required terminal dependencies.
    pub(crate) fn dependency_outcomes(
        &self,
        task_id: TaskId,
    ) -> Result<Vec<DependencyOutcome>, SessionError> {
        let task = self
            .state
            .tasks
            .get(&task_id)
            .ok_or(SessionError::UnknownTask(task_id))?;
        let mut outcomes = Vec::with_capacity(task.dependencies.len());
        for dependency in &task.dependencies {
            let record = self
                .state
                .tasks
                .get(dependency)
                .ok_or(SessionError::UnknownTask(*dependency))?;
            let TaskStatus::Terminal(outcome) = &record.status else {
                return Err(SessionError::Invariant(format!(
                    "dependency {dependency} of task {task_id} is not terminal"
                )));
            };
            outcomes.push(DependencyOutcome {
                task_id: record.id,
                kind: record.kind.clone(),
                outcome: outcome.clone(),
                output: record.output.clone(),
            });
        }
        Ok(outcomes)
    }

    /// The admitted inputs durably bound to `task_id`, in admission order.
    ///
    /// The disposition is the binding, so a task can only see inputs admitted
    /// for it. This is the read the generation kind uses instead of receiving a
    /// session handle or an input id it could widen.
    pub(crate) fn assigned_inputs(&self, task_id: TaskId) -> Vec<crate::Input> {
        self.state
            .inputs
            .values()
            .filter(|input| input.disposition == InputDisposition::Assigned(task_id))
            .map(|input| (**input).clone())
            .collect()
    }

    /// Bounded overview: counts only, no transcript or task payloads.
    #[must_use]
    pub fn summary(&self) -> SessionSummary {
        let state = &self.state;
        let mut tasks = TaskCounts::default();
        for task in state.tasks.values() {
            match task.status {
                TaskStatus::Pending => tasks.pending += 1,
                TaskStatus::Running => tasks.running += 1,
                TaskStatus::Terminal(_) => tasks.terminal += 1,
            }
        }
        SessionSummary {
            session_id: state.session_id,
            root_conversation: state
                .root_conversation
                .expect("initialized session has root conversation"),
            last_commit: state
                .last_commit
                .expect("initialized session has first commit"),
            conversations: state.conversations.len(),
            entries: state.entries.len(),
            inputs: state.inputs.len(),
            tasks,
        }
    }

    /// Read one bounded page of a conversation's fork-visible transcript.
    /// `after` is exclusive and must be visible; a returned cursor stays valid
    /// while the transcript remains append-only at that range.
    pub fn conversation_entries(
        &self,
        conversation_id: ConversationId,
        after: Option<crate::EntryId>,
        limit: usize,
    ) -> Result<EntryPage, SessionError> {
        let visible = self
            .state
            .visible_entries(conversation_id)
            .map_err(|error| SessionError::Invariant(error.to_string()))?;
        let start = match after {
            Some(cursor) => visible
                .iter()
                .position(|entry| entry.id == cursor)
                .map(|position| position + 1)
                .ok_or(SessionError::InvisibleContextReference(cursor))?,
            None => 0,
        };
        if limit == 0 || start >= visible.len() {
            return Ok(EntryPage {
                entries: Vec::new(),
                next: None,
            });
        }
        let end = start.saturating_add(limit).min(visible.len());
        let entries: Vec<_> = visible[start..end].to_vec();
        let next = (end < visible.len()).then(|| entries.last().expect("non-empty page").id);
        Ok(EntryPage { entries, next })
    }

    #[must_use]
    pub fn snapshot(&self) -> SessionSnapshot {
        let state = &self.state;
        SessionSnapshot {
            session_id: state.session_id,
            root_conversation: state
                .root_conversation
                .expect("initialized session has root conversation"),
            last_commit: state
                .last_commit
                .expect("initialized session has first commit"),
            conversations: state.conversations.values().map(|value| **value).collect(),
            entries: state
                .entries
                .values()
                .map(|value| (**value).clone())
                .collect(),
            inputs: state
                .inputs
                .values()
                .map(|value| (**value).clone())
                .collect(),
            tasks: state
                .tasks
                .values()
                .map(|value| (**value).clone())
                .collect(),
        }
    }

    /// Committed observation tail after `cursor`.
    ///
    /// `cursor = None` means "everything still retained" and never asks for a
    /// reset. A cursor older than retained coverage, or one ahead of this
    /// session's last commit (another session, or a reopened store), returns
    /// `reset_required` with no events so the caller resnapshots instead of
    /// silently believing it is current.
    #[must_use]
    pub fn observations_after(&self, cursor: Option<CommitSeq>) -> ObservationBatch {
        let last_commit = self
            .state
            .last_commit
            .expect("initialized session has first commit");
        let reset_required = match cursor {
            None => false,
            Some(cursor) => {
                cursor > last_commit
                    || self
                        .dropped_through
                        .is_some_and(|dropped| cursor <= dropped)
            }
        };
        if reset_required {
            return ObservationBatch {
                reset_required: true,
                events: Vec::new(),
            };
        }

        let events = self
            .observations
            .iter()
            .filter(|event| cursor.is_none_or(|cursor| event.commit_seq > cursor))
            .cloned()
            .collect();
        ObservationBatch {
            reset_required: false,
            events,
        }
    }

    pub(crate) fn fault_signal(&self) -> tokio_util::sync::CancellationToken {
        self.fault.clone()
    }

    pub(crate) fn ensure_open(&self) -> Result<(), SessionError> {
        if self.closed {
            Err(SessionError::Closed)
        } else {
            Ok(())
        }
    }

    pub(crate) fn close(&mut self) {
        self.closed = true;
        self.changes.send_replace(());
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.changes.subscribe()
    }

    pub(crate) fn task_record(&self, task_id: TaskId) -> Option<TaskRecord> {
        self.state.tasks.get(&task_id).map(|task| (**task).clone())
    }

    /// Test-only pointer to a task's resident allocation, used to prove that a
    /// commit does not deep-copy unrelated task payloads.
    #[cfg(test)]
    pub(crate) fn task_record_ptr(&self, task_id: TaskId) -> Option<usize> {
        self.state
            .tasks
            .get(&task_id)
            .map(|task| std::sync::Arc::as_ptr(task) as usize)
    }

    pub(crate) fn set_input_disposition(
        &mut self,
        input_id: InputId,
        disposition: InputDisposition,
    ) -> Result<CommitSeq, SessionError> {
        let (_, commit_seq) =
            self.transact(|transaction| transaction.set_input_disposition(input_id, disposition))?;
        Ok(commit_seq)
    }

    pub(crate) fn reserve_task_invocation(
        &mut self,
        task_id: TaskId,
        kind: InvocationKind,
    ) -> Result<InvocationReceipt, SessionError> {
        let (generation, commit_seq) =
            self.transact(|transaction| transaction.reserve_task(task_id, kind))?;
        Ok(InvocationReceipt {
            generation,
            kind,
            commit_seq,
        })
    }

    pub(crate) fn checkpoint_task(
        &mut self,
        task_id: TaskId,
        generation: u64,
        checkpoint: Option<Value>,
        output: Option<TaskOutput>,
    ) -> Result<CommitSeq, SessionError> {
        let (_, commit_seq) = self.transact(|transaction| {
            transaction.checkpoint_task(task_id, generation, checkpoint, output)
        })?;
        Ok(commit_seq)
    }

    pub(crate) fn mark_task_cancellation(
        &mut self,
        task_id: TaskId,
    ) -> Result<CancellationReceipt, SessionError> {
        self.ensure_open()?;
        let state = &self.state;
        let task = state
            .tasks
            .get(&task_id)
            .ok_or(SessionError::UnknownTask(task_id))?;
        if task.cancel_requested || matches!(task.status, TaskStatus::Terminal(_)) {
            return Ok(CancellationReceipt {
                changed: false,
                commit_seq: state.last_commit.expect("initialized session has a commit"),
            });
        }

        let (_, commit_seq) =
            self.transact(|transaction| transaction.mark_task_cancellation(task_id))?;
        Ok(CancellationReceipt {
            changed: true,
            commit_seq,
        })
    }

    pub(crate) fn settle_task_with<T>(
        &mut self,
        task_id: TaskId,
        generation: u64,
        outcome: TaskOutcome,
        output: Option<TaskOutput>,
        plan: impl FnOnce(&mut Transaction) -> Result<T, SessionError>,
    ) -> Result<(T, CommitSeq), SessionError> {
        self.transact(|transaction| {
            transaction.assert_task_write_authority(task_id, generation)?;
            let value = plan(transaction)?;
            transaction.settle_task(task_id, generation, outcome, output)?;
            Ok(value)
        })
    }

    /// Replay an already-admitted submission: the same input without a new
    /// commit. The turn root is recovered from the binding, so a retried submit
    /// cannot open a second turn for the same input.
    fn replay_submission(
        &self,
        key: &RequestKey,
        request: &InputRequest,
    ) -> Result<Option<SubmissionReceipt>, SessionError> {
        let Some(receipt) = self.replay_input(key, request)? else {
            return Ok(None);
        };
        let task_id =
            self.state
                .inputs
                .get(&receipt.input_id)
                .and_then(|input| match input.disposition {
                    InputDisposition::Assigned(task_id) => Some(task_id),
                    InputDisposition::Queued
                    | InputDisposition::Consumed(_)
                    | InputDisposition::Cancelled => None,
                });
        Ok(Some(SubmissionReceipt {
            input_id: receipt.input_id,
            task_id,
            commit_seq: receipt.commit_seq,
            replayed: true,
        }))
    }

    fn replay_input(
        &self,
        key: &RequestKey,
        request: &InputRequest,
    ) -> Result<Option<InputReceipt>, SessionError> {
        let state = &self.state;
        let Some(input_id) = state.request_keys.get(key).copied() else {
            return Ok(None);
        };
        let existing = state.inputs.get(&input_id).ok_or_else(|| {
            SessionError::Invariant("request key points to missing input".to_owned())
        })?;
        if existing.target != request.target
            || existing.sender != request.sender
            || existing.mode != request.mode
            || existing.body != request.body
        {
            return Err(SessionError::IdempotencyConflict(key.clone()));
        }
        let commit_seq = state.input_commits.get(&input_id).copied().ok_or_else(|| {
            SessionError::Invariant("admitted input is missing its commit sequence".to_owned())
        })?;
        Ok(Some(InputReceipt {
            input_id,
            commit_seq,
            replayed: true,
        }))
    }

    fn transact<T>(
        &mut self,
        build: impl FnOnce(&mut Transaction) -> Result<T, SessionError>,
    ) -> Result<(T, CommitSeq), SessionError> {
        self.ensure_open()?;
        let mut transaction = Transaction::new(&self.state);
        let value = build(&mut transaction)?;
        let (batch, changes, prepared_state) = transaction.finish()?;
        let event = CommitEvent {
            commit_seq: batch.commit_seq,
            changes,
        };
        let commit_seq = event.commit_seq;
        if let Err(error) = self.store.commit(&batch) {
            self.close();
            self.fault.cancel();
            return Err(SessionError::Persistence(error.to_string()));
        }
        // The fully validated draft is installed only after persistence succeeds.
        // Installation cannot introduce a fallible semantic step after durability.
        self.state = prepared_state;
        self.publish(event);
        Ok((value, commit_seq))
    }

    fn publish(&mut self, event: CommitEvent) {
        if self.observations.len() == OBSERVATION_CAPACITY
            && let Some(dropped) = self.observations.pop_front()
        {
            self.dropped_through = Some(dropped.commit_seq);
        }
        self.observations.push_back(event);
        self.changes.send_replace(());
    }
}
