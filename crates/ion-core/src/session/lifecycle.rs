//! Driver lifecycle: graceful and fault close, and abort cleanup admission.
//!
//! Closing fences canonical writes before joining local invocations. Neither
//! mode marks durable cancellation: a task that was running stays running and
//! recoverable, and cleanup is entered explicitly through abort.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::scheduler::{DriveOutcome, InterruptionReason, running_task};
use super::{TaskDriver, TaskDriverError};
use crate::session::command::SessionError;
use crate::task::TaskKind;
use crate::{InvocationKind, TaskId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseMode {
    Graceful,
    Fault,
}

impl TaskDriver {
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
                // Ownership is released last: admission is closed, canonical
                // writes are fenced and every local invocation has joined, so
                // another process may now take the session over.
                self.session.lock().await.release_ownership();
                return;
            }
            drained.changed().await.expect("driver owns drain sender");
        }
    }

    pub(super) async fn cleanup_permit(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, TaskDriverError> {
        tokio::select! {
            biased;
            () = self.stopping.cancelled() => Err(SessionError::Closed.into()),
            () = self.fault.cancelled() => Err(SessionError::Closed.into()),
            permit = self.capacity.acquire_cleanup() => Ok(permit),
        }
    }

    pub(super) async fn run_abort(
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
}
