use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use serde_json::json;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::session::command::InvocationReceipt;
use crate::task::{ContextFuture, TaskRuntime};
use crate::{
    AbortContext, CommitSeq, EntryId, InputDisposition, InputId, InvocationKind, RunningTask,
    Session, SessionError, SessionSnapshot, TaskCompletion, TaskContext, TaskContextError, TaskId,
    TaskKind, TaskOutcome, TaskRecord, TaskRegistry, TaskStatus,
};

#[derive(Clone)]
pub struct TaskDriver {
    session: Arc<Mutex<Session>>,
    registry: Arc<TaskRegistry>,
    active: Arc<StdMutex<HashMap<TaskId, CancellationToken>>>,
}

impl TaskDriver {
    #[must_use]
    pub fn new(session: Session, registry: TaskRegistry) -> Self {
        Self {
            session: Arc::new(Mutex::new(session)),
            registry: Arc::new(registry),
            active: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        self.session.lock().await.snapshot()
    }

    pub async fn drive_task(&self, task_id: TaskId) -> Result<DriveOutcome, TaskDriverError> {
        let active = ActiveInvocation::acquire(self.active.clone(), task_id)?;
        let (task, receipt) = {
            let mut session = self.session.lock().await;
            let task = session
                .task_record(task_id)
                .ok_or(SessionError::UnknownTask(task_id))?;
            let kind = next_invocation(&task).ok_or(TaskDriverError::AlreadyTerminal(task_id))?;
            let receipt = session.reserve_task_invocation(task_id, kind)?;
            (task, receipt)
        };

        let handler = self.registry.get(&task.kind, task.schema_version);
        let running = running_task(task, receipt);
        let completion = match handler.as_ref() {
            Some(handler) => {
                self.run_handler(handler.clone(), running.clone(), active.token())
                    .await
            }
            None => TaskCompletion::unsupported(json!({
                "kind": running.kind.as_str(),
                "schema_version": running.schema_version,
                "reason": "task kind is not registered"
            })),
        };

        if running.invocation_kind == InvocationKind::Abort {
            return self.settle(running, completion).await;
        }

        self.finish_normal(running, completion, handler).await
    }

    pub async fn cancel_task(&self, task_id: TaskId) -> Result<TaskCancellation, TaskDriverError> {
        let receipt = {
            let mut session = self.session.lock().await;
            session.mark_task_cancellation(task_id)?
        };
        if receipt.changed {
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
        Ok(TaskCancellation {
            changed: receipt.changed,
            commit_seq: receipt.commit_seq,
        })
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
        completion: TaskCompletion,
        handler: Option<Arc<dyn TaskKind>>,
    ) -> Result<DriveOutcome, TaskDriverError> {
        let task_id = running.id;
        {
            let mut session = self.session.lock().await;
            let task = session
                .task_record(task_id)
                .ok_or(SessionError::UnknownTask(task_id))?;
            if !task.cancel_requested {
                let TaskCompletion { outcome, output } = completion;
                let (_, commit_seq) = session.settle_task_with(
                    running.id,
                    running.generation,
                    outcome.clone(),
                    output,
                    |_| Ok(()),
                )?;
                return Ok(DriveOutcome {
                    task_id: running.id,
                    invocation_kind: running.invocation_kind,
                    generation: running.generation,
                    reservation_commit: running.reservation_commit,
                    settlement_commit: commit_seq,
                    outcome,
                });
            }
        }

        self.run_abort(task_id, handler).await
    }

    async fn run_abort(
        &self,
        task_id: TaskId,
        handler: Option<Arc<dyn TaskKind>>,
    ) -> Result<DriveOutcome, TaskDriverError> {
        let running = {
            let mut session = self.session.lock().await;
            let receipt = session.reserve_task_invocation(task_id, InvocationKind::Abort)?;
            let task = session
                .task_record(task_id)
                .ok_or(SessionError::UnknownTask(task_id))?;
            running_task(task, receipt)
        };
        let completion = match handler {
            Some(handler) => self.run_abort_handler(handler, running.clone()).await,
            None => TaskCompletion::unsupported(json!({
                "kind": running.kind.as_str(),
                "schema_version": running.schema_version,
                "reason": "task kind is not registered during abort"
            })),
        };
        self.settle(running, completion).await
    }

    async fn run_handler(
        &self,
        handler: Arc<dyn TaskKind>,
        running: RunningTask,
        cancellation: CancellationToken,
    ) -> TaskCompletion {
        let runtime: Arc<dyn TaskRuntime> = Arc::new(SessionTaskRuntime {
            session: self.session.clone(),
        });
        let context = TaskContext::new(runtime, running.id, running.generation, cancellation);
        let invocation_kind = running.invocation_kind;
        let join = tokio::spawn(async move {
            match invocation_kind {
                InvocationKind::Execute => handler.execute(running, context).await,
                InvocationKind::Recover => handler.recover(running, context).await,
                InvocationKind::Abort => unreachable!("abort uses restricted context"),
            }
        })
        .await;
        completion_from_join(join)
    }

    async fn run_abort_handler(
        &self,
        handler: Arc<dyn TaskKind>,
        running: RunningTask,
    ) -> TaskCompletion {
        let runtime: Arc<dyn TaskRuntime> = Arc::new(SessionTaskRuntime {
            session: self.session.clone(),
        });
        let context = AbortContext::new(runtime, running.id, running.generation);
        let join = tokio::spawn(async move { handler.abort(running, context).await }).await;
        completion_from_join(join)
    }

    async fn settle(
        &self,
        running: RunningTask,
        completion: TaskCompletion,
    ) -> Result<DriveOutcome, TaskDriverError> {
        let TaskCompletion { outcome, output } = completion;
        let (_, commit_seq) = {
            let mut session = self.session.lock().await;
            session.settle_task_with(
                running.id,
                running.generation,
                outcome.clone(),
                output,
                |_| Ok(()),
            )?
        };
        Ok(DriveOutcome {
            task_id: running.id,
            invocation_kind: running.invocation_kind,
            generation: running.generation,
            reservation_commit: running.reservation_commit,
            settlement_commit: commit_seq,
            outcome,
        })
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
}

fn context_error(error: SessionError) -> TaskContextError {
    match error {
        SessionError::CancellationFence(_) => TaskContextError::Cancelled,
        SessionError::StaleInvocation { .. } => TaskContextError::Stale,
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

fn completion_from_join(
    join: Result<Result<TaskCompletion, crate::TaskRunError>, tokio::task::JoinError>,
) -> TaskCompletion {
    match join {
        Ok(Ok(completion)) => completion,
        Ok(Err(error)) => TaskCompletion::failed(json!({"error": error.message()})),
        Err(error) => TaskCompletion::failed(json!({
            "error": if error.is_panic() { "task panicked" } else { "task join failed" }
        })),
    }
}

struct ActiveInvocation {
    active: Arc<StdMutex<HashMap<TaskId, CancellationToken>>>,
    task_id: TaskId,
    token: CancellationToken,
}

impl ActiveInvocation {
    fn acquire(
        active: Arc<StdMutex<HashMap<TaskId, CancellationToken>>>,
        task_id: TaskId,
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
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DriveOutcome {
    pub task_id: TaskId,
    pub invocation_kind: InvocationKind,
    pub generation: u64,
    pub reservation_commit: CommitSeq,
    pub settlement_commit: CommitSeq,
    pub outcome: TaskOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskCancellation {
    pub changed: bool,
    pub commit_seq: CommitSeq,
}

#[derive(Debug, Error)]
pub enum TaskDriverError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("task {0} already has a live local invocation")]
    AlreadyActive(TaskId),
    #[error("task {0} is already terminal")]
    AlreadyTerminal(TaskId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConversationId, TaskKindName, TaskOutcomeKind};

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
}
