//! Retained leaf I/O uses the same completion-custody protocol as execution,
//! without admitting another Turn or introducing another durable work queue.
use std::{
    io,
    sync::{Arc, Mutex},
};

use crate::operation::{OperationError, OperationFailure, OperationOwner};

#[derive(Default)]
pub(crate) struct CaptureWork(Mutex<State>);
#[derive(Default)]
struct State {
    closed: bool,
    owners: Vec<Arc<OperationOwner>>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum CaptureError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Operation(#[from] OperationError),
}
impl OperationFailure for CaptureError {
    fn expected_cancellation(&self) -> bool {
        false
    }
}

impl CaptureWork {
    pub(super) async fn run<T: Send + 'static>(
        &self,
        owner: &Arc<OperationOwner>,
        work: impl FnOnce() -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        let operation = {
            let state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.closed {
                return Err(io::Error::other("Session artifact capture is closed"));
            }
            owner.start_blocking::<T, CaptureError>(
                tokio_util::sync::CancellationToken::new(),
                move || work().map_err(CaptureError::Io),
            )
        };
        operation
            .await
            .map_err(|error| io::Error::other(format!("artifact storage: {error}")))
    }

    pub(super) fn reserve_sync<'a>(
        &self,
        owner: &'a OperationOwner,
    ) -> io::Result<crate::IdleSession<'a>> {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(io::Error::other("Session artifact capture is closed"));
        }
        owner
            .try_lock()
            .map_err(|error| io::Error::other(error.to_string()))
    }

    pub(crate) fn register(&self) -> io::Result<Arc<OperationOwner>> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(io::Error::other("Session artifact capture is closed"));
        }
        // Keep live and abandoned-failure custody; discard only reported or
        // successful work once no writer/waiter owns it. This is not an archive.
        state
            .owners
            .retain(|owner| Arc::strong_count(owner) > 1 || owner.try_lock().is_err());
        let owner = Arc::new(OperationOwner::default());
        state.owners.push(owner.clone());
        Ok(owner)
    }

    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        let owners = {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            state.owners.clone()
        };
        // Join all owners even when an earlier one failed. Cancellation of this
        // wait leaves the registry and each actual task in retained custody.
        let mut errors = Vec::new();
        for owner in owners {
            if let Err(error) = owner.shutdown().await {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}
