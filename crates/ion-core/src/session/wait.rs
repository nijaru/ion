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

    /// Wait until a foreground turn has completed, and return the member that
    /// closed it.
    ///
    /// This is the client-side wait for "the worker's whole chain answered",
    /// which is not the same as waiting for its initial generation: a generation
    /// settles as soon as it has planned its children. Dropping this future only
    /// drops this wait, and it holds no writer or capacity permit while waiting.
    pub async fn wait_turn(&self, root: TaskId) -> Result<TaskId, TaskDriverError> {
        let mut changes = {
            let session = self.session.lock().await;
            session.ensure_open()?;
            let task = session
                .task_record(root)
                .ok_or(SessionError::UnknownTask(root))?;
            if task.turn != Some(root) {
                return Err(SessionError::NotATurnRoot(root).into());
            }
            session.subscribe()
        };
        loop {
            {
                let session = self.session.lock().await;
                session.ensure_open()?;
                if let Some(closed_by) = session.turn_closed_by(root) {
                    return Ok(closed_by);
                }
            }
            changes
                .changed()
                .await
                .expect("driver owns session notification sender");
        }
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
                    || (!terminal && session.ready_to_run(&task));
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
