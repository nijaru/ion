use crate::{SessionError, TaskId, TaskRecord, TaskStatus};

use super::{TaskDriver, TaskDriverError};

impl TaskDriver {
    /// Wait for committed terminal state. Dropping this future only drops this wait.
    /// This is a client API, not an invocation-scoped task-to-task wait.
    pub async fn wait_task(&self, task_id: TaskId) -> Result<TaskRecord, TaskDriverError> {
        self.wait_state(task_id, true).await
    }

    /// Wait until fixed dependencies are terminal, or cancellation makes abort eligible.
    /// This does not reserve an invocation or acquire resource capacity.
    pub async fn wait_dependencies(&self, task_id: TaskId) -> Result<TaskRecord, TaskDriverError> {
        self.wait_state(task_id, false).await
    }

    async fn wait_state(
        &self,
        task_id: TaskId,
        terminal: bool,
    ) -> Result<TaskRecord, TaskDriverError> {
        // Subscribe before reading. Notifications are wake signals only; coalescing
        // is safe because every wake rechecks the canonical state under the writer.
        let mut changes = self.session.lock().await.subscribe();
        loop {
            {
                let session = self.session.lock().await;
                session.ensure_open()?;
                let task = session
                    .task_record(task_id)
                    .ok_or(SessionError::UnknownTask(task_id))?;
                let ready = matches!(task.status, TaskStatus::Terminal(_))
                    || (!terminal
                        && (task.cancel_requested
                            || task.dependencies.iter().all(|id| {
                                session.task_record(*id).is_some_and(|dependency| {
                                    matches!(dependency.status, TaskStatus::Terminal(_))
                                })
                            })));
                if ready {
                    return Ok(task);
                }
            }
            changes
                .changed()
                .await
                .expect("driver owns session notification sender");
        }
    }
}
