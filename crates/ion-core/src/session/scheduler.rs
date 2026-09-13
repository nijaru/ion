use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use serde_json::json;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::ResourceDomain;
use crate::session::command::{InvocationReceipt, TurnCancellation};
use crate::task::{ContextFuture, TaskRuntime};
use crate::{
    AbortContext, CommitSeq, EntryId, InputDisposition, InputId, InvocationKind, ObservationBatch,
    RunningTask, Session, SessionError, SessionSnapshot, SessionSummary, TaskCompletion,
    TaskContext, TaskContextError, TaskId, TaskKind, TaskOutcome, TaskRecord, TaskRegistry,
    TaskRunError, TaskStatus,
};

#[derive(Clone)]
pub struct TaskDriver {
    pub(super) session: Arc<Mutex<Session>>,
    registry: Arc<std::sync::RwLock<TaskRegistry>>,
    capacity: super::TaskCapacity,
    active: Arc<StdMutex<HashMap<TaskId, CancellationToken>>>,
    drained: tokio::sync::watch::Sender<()>,
    changes: tokio::sync::watch::Receiver<()>,
    stopping: CancellationToken,
    fault: CancellationToken,
}

impl TaskDriver {
    #[must_use]
    pub fn new(session: Session, registry: TaskRegistry) -> Self {
        Self::with_capacity(session, registry, super::TaskCapacity::default())
    }

    #[must_use]
    pub fn with_capacity(
        session: Session,
        registry: TaskRegistry,
        capacity: super::TaskCapacity,
    ) -> Self {
        let fault = session.fault_signal();
        let changes = session.subscribe();
        Self {
            capacity,
            session: Arc::new(Mutex::new(session)),
            registry: Arc::new(std::sync::RwLock::new(registry)),
            active: Arc::new(StdMutex::new(HashMap::new())),
            drained: tokio::sync::watch::channel(()).0,
            changes,
            stopping: CancellationToken::new(),
            fault,
        }
    }

    /// Restore a missing implementation without touching durable task data.
    /// A previously running task stays blocked until its kind is registered.
    pub fn register_task_kind(
        &self,
        kind: crate::TaskKindName,
        schema_version: u32,
        handler: Arc<dyn TaskKind>,
    ) -> Result<(), crate::TaskRegistryError> {
        self.registry
            .write()
            .expect("task registry lock")
            .register(kind, schema_version, handler)
    }

    fn handler_for(&self, task: &TaskRecord) -> Result<Option<Arc<dyn TaskKind>>, TaskDriverError> {
        let handler = self
            .registry
            .read()
            .expect("task registry lock")
            .get(&task.kind, task.schema_version);
        if handler.is_none() && matches!(task.status, TaskStatus::Running) {
            return Err(TaskDriverError::RecoveryBlocked {
                task_id: task.id,
                kind: task.kind.clone(),
                schema_version: task.schema_version,
            });
        }
        Ok(handler)
    }

    fn resource_domain(&self, task: &TaskRecord) -> Option<ResourceDomain> {
        self.registry
            .read()
            .expect("task registry lock")
            .get(&task.kind, task.schema_version)
            .and_then(|handler| handler.resource_domain())
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        self.session.lock().await.snapshot()
    }

    /// Bounded overview: counts and cursors only. A live client should prefer
    /// this plus [`Self::observations_after`] over [`Self::snapshot`].
    pub async fn summary(&self) -> SessionSummary {
        self.session.lock().await.summary()
    }

    /// Look up one task record without materializing any other record.
    pub async fn task(&self, task_id: TaskId) -> Option<TaskRecord> {
        self.session.lock().await.task_record(task_id)
    }

    /// Read one bounded page of a conversation's fork-visible transcript.
    pub async fn conversation_entries(
        &self,
        conversation_id: crate::ConversationId,
        after: Option<EntryId>,
        limit: usize,
    ) -> Result<crate::EntryPage, SessionError> {
        self.session
            .lock()
            .await
            .conversation_entries(conversation_id, after, limit)
    }

    /// Committed observation tail after `cursor`. `reset_required` means the
    /// caller's cursor is outside retained coverage and it must resnapshot.
    pub async fn observations_after(&self, cursor: Option<CommitSeq>) -> ObservationBatch {
        self.session.lock().await.observations_after(cursor)
    }

    /// Resolve when a change has been committed since this call started. This
    /// is the polling-free companion to [`Self::observations_after`]; it
    /// carries no payload, so a waiter still reads the tail itself.
    pub async fn changed(&self) {
        let _ = self.changes.clone().changed().await;
    }

    pub async fn drive_task(&self, task_id: TaskId) -> Result<DriveOutcome, TaskDriverError> {
        // Admission and close are serialized by the session writer. The owned
        // drive outlives its caller so dropping a receipt cannot orphan a handler.
        let session = self.session.lock().await;
        session.ensure_open()?;
        let active = ActiveInvocation::acquire(self.active.clone(), task_id, self.drained.clone())?;
        let drive = self.drive_owned(task_id, active);
        drop(session);
        tokio::spawn(drive)
            .await
            .map_err(|error| TaskDriverError::DriverJoin(error.to_string()))?
    }

    /// An owned drive, as an opaque boxed future.
    ///
    /// Readiness dispatch runs inside a drive and spawns further drives. Boxing
    /// the future keeps that recursion out of the type of the enclosing future,
    /// which the compiler otherwise cannot prove `Send`.
    fn drive_owned(&self, task_id: TaskId, active: ActiveInvocation) -> BoxDrive {
        let driver = self.clone();
        Box::pin(async move { driver.drive_owned_inner(task_id, active).await })
    }

    async fn drive_owned_inner(
        &self,
        task_id: TaskId,
        active: ActiveInvocation,
    ) -> Result<DriveOutcome, TaskDriverError> {
        let eligible = self.wait_dependencies(task_id).await?;
        let mut cleanup = eligible.cancel_requested;
        let cancellation = active.token();
        let (task, receipt, handler, permit) = loop {
            let permit = if cleanup {
                Some(self.cleanup_permit().await?)
            } else {
                tokio::select! {
                    biased;
                    () = self.stopping.cancelled() => return Err(SessionError::Closed.into()),
                    () = self.fault.cancelled() => return Err(SessionError::Closed.into()),
                    () = cancellation.cancelled() => { cleanup = true; continue; }
                    permit = self.capacity.acquire(self.resource_domain(&eligible)) => permit,
                }
            };
            let mut session = self.session.lock().await;
            session.ensure_open()?;
            let task = session
                .task_record(task_id)
                .ok_or(SessionError::UnknownTask(task_id))?;
            let kind = next_invocation(&task).ok_or(TaskDriverError::AlreadyTerminal(task_id))?;
            // Cancellation may commit after normal admission but before reservation.
            // Release the normal permit and reclassify before reserving Abort.
            if kind == InvocationKind::Abort && !cleanup {
                cleanup = true;
                continue;
            }
            let handler = self.handler_for(&task)?;
            let receipt = session.reserve_task_invocation(task_id, kind)?;
            break (task, receipt, handler, permit);
        };

        let running = running_task(task, receipt);
        // A pending kind with no registered implementation settles Unsupported
        // without losing its record. A running task never reaches here:
        // `handler_for` blocks recovery instead of fabricating terminal state.
        let Some(handler) = handler else {
            let completion = TaskCompletion::unsupported(json!({
                "kind": running.kind.as_str(),
                "schema_version": running.schema_version,
                "reason": "task kind is not registered"
            }));
            return self.settle(running, Ok(completion)).await;
        };

        let completion = self
            .run_handler(handler.clone(), running.clone(), active.token())
            .await;

        if running.invocation_kind == InvocationKind::Abort {
            return self.settle(running, completion).await;
        }

        drop(permit);
        self.finish_normal(running, completion, Some(handler)).await
    }

    /// Stop admission and canonical writes, then join all local drives.
    /// Graceful close asks normal handlers to return cooperatively; fault close
    /// drops their futures after fencing. Neither marks durable cancellation.
    pub async fn close(&self, mode: CloseMode) {
        let mut drained = self.drained.subscribe();
        {
            let mut session = self.session.lock().await;
            session.close();
            self.stopping.cancel();
            if mode == CloseMode::Fault {
                self.fault.cancel();
            }
            for token in self.active.lock().expect("active task mutex").values() {
                token.cancel();
            }
        }
        loop {
            if self.active.lock().expect("active task mutex").is_empty() {
                return;
            }
            drained.changed().await.expect("driver owns drain sender");
        }
    }

    pub async fn cancel_task(&self, task_id: TaskId) -> Result<TaskCancellation, TaskDriverError> {
        let receipt = {
            let mut session = self.session.lock().await;
            session.mark_task_cancellation(task_id)?
        };
        if receipt.changed {
            self.signal(task_id);
        }
        Ok(TaskCancellation {
            changed: receipt.changed,
            commit_seq: receipt.commit_seq,
        })
    }

    /// Cancel one foreground turn: the durable marks commit first, then every
    /// affected local invocation is signalled. Retained workers and background
    /// tasks whose `turn` is unset are outside this scope.
    pub async fn cancel_turn(&self, root: TaskId) -> Result<TurnCancellation, TaskDriverError> {
        let cancellation = {
            let mut session = self.session.lock().await;
            session.cancel_turn(root)?
        };
        for task_id in &cancellation.cancelled {
            self.signal(*task_id);
        }
        Ok(cancellation)
    }

    fn signal(&self, task_id: TaskId) {
        let token = self
            .active
            .lock()
            .expect("active task mutex")
            .get(&task_id)
            .cloned();
        if let Some(token) = token {
            token.cancel();
        }
    }

    /// Admit a new foreground turn on its conversation. Rejected while that
    /// conversation already has a live turn chain.
    pub async fn create_turn(
        &self,
        request: crate::TaskRequest,
    ) -> Result<crate::TaskReceipt, TaskDriverError> {
        let mut session = self.session.lock().await;
        Ok(session.create_turn(request)?)
    }

    pub async fn assign_input(
        &self,
        input_id: InputId,
        task_id: TaskId,
    ) -> Result<CommitSeq, TaskDriverError> {
        let mut session = self.session.lock().await;
        Ok(session.set_input_disposition(input_id, InputDisposition::Assigned(task_id))?)
    }

    pub async fn consume_input(
        &self,
        input_id: InputId,
        entry_id: EntryId,
    ) -> Result<CommitSeq, TaskDriverError> {
        let mut session = self.session.lock().await;
        Ok(session.set_input_disposition(input_id, InputDisposition::Consumed(entry_id))?)
    }

    async fn finish_normal(
        &self,
        running: RunningTask,
        completion: Result<TaskCompletion, InterruptionReason>,
        handler: Option<Arc<dyn TaskKind>>,
    ) -> Result<DriveOutcome, TaskDriverError> {
        let task_id = running.id;
        let settled = {
            let mut session = self.session.lock().await;
            session.ensure_open()?;
            let task = session
                .task_record(task_id)
                .ok_or(SessionError::UnknownTask(task_id))?;
            if task.cancel_requested {
                None
            } else {
                match completion {
                    Ok(completion) => {
                        Some(self.settle_in_session(&mut session, &running, completion))
                    }
                    Err(reason) => {
                        return Ok(DriveOutcome::Interrupted(Interruption {
                            task_id: running.id,
                            invocation_kind: running.invocation_kind,
                            generation: running.generation,
                            reservation_commit: running.reservation_commit,
                            reason,
                        }));
                    }
                }
            }
        };

        match settled {
            Some(Ok((outcome, created))) => {
                // Work this settlement unblocked becomes runnable here, and only
                // here: admission never starts work on its own.
                self.dispatch_runnable(task_id, &created).await;
                Ok(outcome)
            }
            Some(Err(error)) => Err(error),
            // Cancellation committed during the normal invocation; the old
            // invocation already joined, so cleanup runs as a fresh abort generation.
            None => self.run_abort(task_id, handler).await,
        }
    }

    /// Drive the successors a settlement left runnable and that have a
    /// registered kind.
    ///
    /// Readiness alone never fabricates an outcome: a candidate with no
    /// registered implementation stays pending for an explicit drive, so
    /// registering a kind later is still possible. Each candidate runs as an
    /// ordinary owned invocation, which means close still joins it, a second
    /// local drive for one task is still rejected, and cancellation still
    /// fences reservation.
    async fn dispatch_runnable(&self, settled: TaskId, created: &[TaskId]) {
        let candidates = {
            let session = self.session.lock().await;
            if session.ensure_open().is_err() {
                return;
            }
            session.runnable_successors(settled, created)
        };
        let registry = self.registry.read().expect("task registry lock");
        let dispatchable: Vec<TaskId> = candidates
            .into_iter()
            .filter(|candidate| {
                registry
                    .get(&candidate.kind, candidate.schema_version)
                    .is_some()
            })
            .map(|candidate| candidate.id)
            .collect();
        drop(registry);

        for task_id in dispatchable {
            let Ok(active) =
                ActiveInvocation::acquire(self.active.clone(), task_id, self.drained.clone())
            else {
                // Already driven locally; the existing drive owns it.
                continue;
            };
            let driver = self.clone();
            let spawned: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
                Box::pin(async move {
                    let _ = driver.drive_owned(task_id, active).await;
                });
            tokio::spawn(spawned);
        }
    }

    async fn cleanup_permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit, TaskDriverError> {
        tokio::select! {
            biased;
            () = self.stopping.cancelled() => Err(SessionError::Closed.into()),
            () = self.fault.cancelled() => Err(SessionError::Closed.into()),
            permit = self.capacity.acquire_cleanup() => Ok(permit),
        }
    }

    async fn run_abort(
        &self,
        task_id: TaskId,
        handler: Option<Arc<dyn TaskKind>>,
    ) -> Result<DriveOutcome, TaskDriverError> {
        let _permit = self.cleanup_permit().await?;
        let running = {
            let mut session = self.session.lock().await;
            session.ensure_open()?;
            let receipt = session.reserve_task_invocation(task_id, InvocationKind::Abort)?;
            let task = session
                .task_record(task_id)
                .ok_or(SessionError::UnknownTask(task_id))?;
            running_task(task, receipt)
        };
        // Cleanup requires the registered kind. Without it the task stays
        // durably running and cancelled for a later explicit abort drive.
        let completion = match handler {
            Some(handler) => {
                self.run_handler(handler, running.clone(), CancellationToken::new())
                    .await
            }
            None => Err(InterruptionReason::HandlerUnavailable),
        };
        self.settle(running, completion).await
    }

    async fn run_handler(
        &self,
        handler: Arc<dyn TaskKind>,
        running: RunningTask,
        cancellation: CancellationToken,
    ) -> Result<TaskCompletion, InterruptionReason> {
        let runtime: Arc<dyn TaskRuntime> = Arc::new(SessionTaskRuntime {
            session: self.session.clone(),
        });
        let mut join = tokio::spawn(async move {
            match running.invocation_kind {
                InvocationKind::Execute | InvocationKind::Recover => {
                    let context =
                        TaskContext::new(runtime, running.id, running.generation, cancellation);
                    if running.invocation_kind == InvocationKind::Execute {
                        handler.execute(running, context).await
                    } else {
                        handler.recover(running, context).await
                    }
                }
                InvocationKind::Abort => {
                    let context = AbortContext::new(runtime, running.id, running.generation);
                    handler.abort(running, context).await
                }
            }
        });
        let result = tokio::select! {
            result = &mut join => result,
            () = self.fault.cancelled() => { join.abort(); join.await }
        };
        match result {
            Ok(Ok(completion)) => Ok(completion),
            Ok(Err(TaskRunError::Interrupted(message))) => {
                Err(InterruptionReason::HandlerFailed(message))
            }
            Ok(Err(TaskRunError::Runtime(error))) => Err(InterruptionReason::Runtime(error)),
            Err(join) if join.is_panic() => Err(InterruptionReason::HandlerPanicked),
            Err(join) => Err(InterruptionReason::HandlerFailed(join.to_string())),
        }
    }

    fn settle_in_session(
        &self,
        session: &mut Session,
        running: &RunningTask,
        completion: TaskCompletion,
    ) -> Result<(DriveOutcome, Vec<TaskId>), TaskDriverError> {
        let TaskCompletion {
            outcome,
            output,
            plan,
        } = completion;
        let (created, commit_seq) = session.settle_task_with(
            running.id,
            running.generation,
            outcome.clone(),
            output,
            |transaction| transaction.apply_task_plan(&plan, running.id),
        )?;
        Ok((
            DriveOutcome::Settled(Settlement {
                task_id: running.id,
                invocation_kind: running.invocation_kind,
                generation: running.generation,
                reservation_commit: running.reservation_commit,
                settlement_commit: commit_seq,
                outcome,
            }),
            created,
        ))
    }

    async fn settle(
        &self,
        running: RunningTask,
        completion: Result<TaskCompletion, InterruptionReason>,
    ) -> Result<DriveOutcome, TaskDriverError> {
        match completion {
            Ok(completion) => {
                let settled = {
                    let mut session = self.session.lock().await;
                    self.settle_in_session(&mut session, &running, completion)
                };
                let (outcome, created) = settled?;
                self.dispatch_runnable(running.id, &created).await;
                Ok(outcome)
            }
            // No durable settlement: the task remains running and is retried
            // through an explicit Recover or Abort drive.
            Err(reason) => Ok(DriveOutcome::Interrupted(Interruption {
                task_id: running.id,
                invocation_kind: running.invocation_kind,
                generation: running.generation,
                reservation_commit: running.reservation_commit,
                reason,
            })),
        }
    }
}

struct SessionTaskRuntime {
    session: Arc<Mutex<Session>>,
}

impl TaskRuntime for SessionTaskRuntime {
    fn checkpoint<'a>(
        &'a self,
        task_id: TaskId,
        generation: u64,
        checkpoint: Option<serde_json::Value>,
        output: Option<crate::TaskOutput>,
    ) -> ContextFuture<'a, Result<CommitSeq, TaskContextError>> {
        Box::pin(async move {
            let mut session = self.session.lock().await;
            session
                .checkpoint_task(task_id, generation, checkpoint, output)
                .map_err(context_error)
        })
    }

    fn conversation_entries<'a>(
        &'a self,
        task_id: TaskId,
        after: Option<EntryId>,
        limit: usize,
    ) -> ContextFuture<'a, Result<crate::EntryPage, TaskContextError>> {
        Box::pin(async move {
            let session = self.session.lock().await;
            session.ensure_open().map_err(context_error)?;
            let conversation_id = session
                .task_record(task_id)
                .ok_or_else(|| TaskContextError::Runtime(format!("unknown task {task_id}")))?
                .conversation_id;
            session
                .conversation_entries(conversation_id, after, limit)
                .map_err(context_error)
        })
    }

    fn dependency_outcomes<'a>(
        &'a self,
        task_id: TaskId,
    ) -> ContextFuture<'a, Result<Vec<crate::DependencyOutcome>, TaskContextError>> {
        Box::pin(async move {
            let session = self.session.lock().await;
            session.ensure_open().map_err(context_error)?;
            session.dependency_outcomes(task_id).map_err(context_error)
        })
    }
}

fn context_error(error: SessionError) -> TaskContextError {
    match error {
        SessionError::CancellationFence(_) => TaskContextError::Cancelled,
        SessionError::StaleInvocation { .. } => TaskContextError::Stale,
        SessionError::Closed => TaskContextError::Closed,
        SessionError::Persistence(message) => TaskContextError::Persistence(message),
        other => TaskContextError::Runtime(other.to_string()),
    }
}

fn running_task(task: TaskRecord, receipt: InvocationReceipt) -> RunningTask {
    RunningTask {
        id: task.id,
        conversation_id: task.conversation_id,
        kind: task.kind,
        schema_version: task.schema_version,
        input: task.input,
        checkpoint: task.checkpoint,
        output: task.output,
        generation: receipt.generation,
        invocation_kind: receipt.kind,
        reservation_commit: receipt.commit_seq,
    }
}

fn next_invocation(task: &TaskRecord) -> Option<InvocationKind> {
    if matches!(task.status, TaskStatus::Terminal(_)) {
        None
    } else if task.cancel_requested {
        Some(InvocationKind::Abort)
    } else {
        match task.status {
            TaskStatus::Pending => Some(InvocationKind::Execute),
            TaskStatus::Running => Some(InvocationKind::Recover),
            TaskStatus::Terminal(_) => None,
        }
    }
}

/// An owned drive future. See `TaskDriver::drive_owned`.
type BoxDrive = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<DriveOutcome, TaskDriverError>> + Send>,
>;

struct ActiveInvocation {
    active: Arc<StdMutex<HashMap<TaskId, CancellationToken>>>,
    task_id: TaskId,
    token: CancellationToken,
    drained: tokio::sync::watch::Sender<()>,
}

impl ActiveInvocation {
    fn acquire(
        active: Arc<StdMutex<HashMap<TaskId, CancellationToken>>>,
        task_id: TaskId,
        drained: tokio::sync::watch::Sender<()>,
    ) -> Result<Self, TaskDriverError> {
        let token = CancellationToken::new();
        {
            let mut active_tasks = active.lock().expect("active task mutex");
            if active_tasks.contains_key(&task_id) {
                return Err(TaskDriverError::AlreadyActive(task_id));
            }
            active_tasks.insert(task_id, token.clone());
        }
        Ok(Self {
            active,
            task_id,
            token,
            drained,
        })
    }

    fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for ActiveInvocation {
    fn drop(&mut self) {
        self.active
            .lock()
            .expect("active task mutex")
            .remove(&self.task_id);
        self.drained.send_replace(());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseMode {
    Graceful,
    Fault,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DriveOutcome {
    /// The task reached a durable terminal outcome.
    Settled(Settlement),
    /// The invocation was interrupted or left unresolved external uncertainty.
    /// No terminal state was written; the durable task is still recoverable.
    Interrupted(Interruption),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settlement {
    pub task_id: TaskId,
    pub invocation_kind: InvocationKind,
    pub generation: u64,
    pub reservation_commit: CommitSeq,
    pub settlement_commit: CommitSeq,
    pub outcome: TaskOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interruption {
    pub task_id: TaskId,
    pub invocation_kind: InvocationKind,
    pub generation: u64,
    pub reservation_commit: CommitSeq,
    pub reason: InterruptionReason,
}

/// A process-local interruption, never a durable terminal outcome. A handler
/// that detects real external uncertainty must instead return an
/// `Indeterminate` completion with retained evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterruptionReason {
    HandlerFailed(String),
    HandlerPanicked,
    HandlerUnavailable,
    Runtime(TaskContextError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskCancellation {
    pub changed: bool,
    pub commit_seq: CommitSeq,
}

#[derive(Debug, Error)]
pub enum TaskDriverError {
    #[error("local task driver failed to join: {0}")]
    DriverJoin(String),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("task {0} already has a live local invocation")]
    AlreadyActive(TaskId),
    #[error("task {0} is already terminal")]
    AlreadyTerminal(TaskId),
    #[error(
        "task {task_id} is running but {kind} v{schema_version} is not registered; recovery is blocked"
    )]
    RecoveryBlocked {
        task_id: TaskId,
        kind: crate::TaskKindName,
        schema_version: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConversationId, TaskFuture, TaskKindName, TaskOutcomeKind};

    fn task(status: TaskStatus, cancel_requested: bool) -> TaskRecord {
        let mut task = TaskRecord::pending(
            TaskId::new(2).expect("task id"),
            ConversationId::new(1).expect("conversation id"),
            TaskKindName::new("test").expect("kind"),
            1,
            serde_json::Value::Null,
            Vec::new(),
        );
        task.status = status;
        task.cancel_requested = cancel_requested;
        task
    }

    #[test]
    fn invocation_selection_distinguishes_execute_recover_abort_and_terminal() {
        assert_eq!(
            next_invocation(&task(TaskStatus::Pending, false)),
            Some(InvocationKind::Execute)
        );
        assert_eq!(
            next_invocation(&task(TaskStatus::Running, false)),
            Some(InvocationKind::Recover)
        );
        assert_eq!(
            next_invocation(&task(TaskStatus::Running, true)),
            Some(InvocationKind::Abort)
        );
        assert_eq!(
            next_invocation(&task(
                TaskStatus::Terminal(TaskOutcome {
                    kind: TaskOutcomeKind::Completed,
                    value: serde_json::Value::Null,
                }),
                false,
            )),
            None
        );
    }

    struct Completing;

    impl TaskKind for Completing {
        fn execute<'a>(&'a self, _task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
            Box::pin(async { Ok(TaskCompletion::completed(serde_json::json!("done"))) })
        }

        fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
            self.execute(task, context)
        }

        fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
            Box::pin(async { Ok(TaskCompletion::aborted(serde_json::json!("aborted"))) })
        }
    }

    #[tokio::test]
    async fn running_task_with_missing_kind_blocks_recovery_until_registered() {
        let mut session = Session::new().expect("session");
        let kind = TaskKindName::new("later").expect("kind");
        let task = session
            .create_task(crate::session::command::TaskRequest {
                conversation_id: session.root_conversation(),
                kind: kind.clone(),
                schema_version: 1,
                input: serde_json::Value::Null,
                dependencies: Vec::new(),
            })
            .expect("task");
        // Simulate a durable running task recovered from storage without its kind.
        session
            .reserve_task_invocation(task.task_id, InvocationKind::Execute)
            .expect("reserve");

        let driver = TaskDriver::new(session, TaskRegistry::new());
        let error = driver
            .drive_task(task.task_id)
            .await
            .expect_err("recovery must be blocked");
        assert!(matches!(error, TaskDriverError::RecoveryBlocked { .. }));

        // Blocked recovery fabricates no terminal state and consumes no generation.
        let snapshot = driver.snapshot().await;
        assert!(matches!(snapshot.tasks[0].status, TaskStatus::Running));
        assert_eq!(snapshot.tasks[0].generation, 1);
        assert!(!snapshot.tasks[0].cancel_requested);

        // Registering the missing implementation restores explicit recovery.
        driver
            .register_task_kind(kind, 1, Arc::new(Completing))
            .expect("register");
        match driver.drive_task(task.task_id).await.expect("drive") {
            DriveOutcome::Settled(settlement) => {
                assert_eq!(settlement.invocation_kind, InvocationKind::Recover);
                assert_eq!(settlement.generation, 2);
                assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Completed);
            }
            other => panic!("expected settlement, got {other:?}"),
        }
    }
}
