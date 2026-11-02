//! Session-owned custody of blocking storage work. This is neither another
//! writer nor a durable work queue: all lanes use the existing Session Store.
use std::sync::{Arc, Mutex};

use crate::operation::{OperationError, OperationFailure, OperationOwner};

use super::{Session, SessionError};

/// A serial lane for blocking Session operations on Tokio's blocking pool.
///
/// Each lane retains one actual operation at a time. Overlapping calls, including
/// reuse while an abandoned call is still running, fail with `OperationActive`;
/// create a separate lane for independent concurrent tasks. Dropping a wait does
/// not cancel its storage work. Unread failures prevent reuse and are reported by
/// Session shutdown, which joins the actual blocking tasks before returning.
///
/// The lane keeps the Session writer lease alive. Session shutdown closes both
/// existing lanes and the creation of new lanes; synchronous operator inspection
/// remains available afterward.
pub struct SessionIo {
    session: Arc<Session>,
    owner: Arc<OperationOwner>,
}

impl SessionIo {
    pub(crate) fn session(&self) -> &Arc<Session> {
        &self.session
    }

    pub(super) fn new(session: &Arc<Session>) -> Result<Self, SessionError> {
        Ok(Self {
            session: session.clone(),
            owner: session.storage.register()?,
        })
    }

    /// Run the entire blocking operation, including lock acquisition, validation,
    /// reads, commits and publication, off the async executor. Admission occurs
    /// when this future is polled, not when it is constructed.
    pub async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Session) -> Result<T, SessionError> + Send + 'static,
    ) -> Result<T, SessionError> {
        self.run_with(work).await
    }

    pub(crate) async fn run_with<T, E>(
        &self,
        work: impl FnOnce(&Session) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: Send + OperationFailure + From<OperationError> + 'static,
    {
        let session = self.session.clone();
        self.session
            .storage
            .run(&self.owner, move || work(&session))
            .await
    }
}

#[derive(Default)]
pub(super) struct StorageWork(Mutex<State>);

#[derive(Default)]
struct State {
    closed: bool,
    owners: Vec<Arc<OperationOwner>>,
}

impl StorageWork {
    pub(super) async fn run<T, E>(
        &self,
        owner: &Arc<OperationOwner>,
        work: impl FnOnce() -> Result<T, E> + Send + 'static,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: Send + OperationFailure + From<OperationError> + 'static,
    {
        let operation = {
            let state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.closed {
                return Err(OperationError::Closed.into());
            }
            owner.start_blocking(tokio_util::sync::CancellationToken::new(), work)
        };
        operation.await
    }

    pub(super) fn reserve_sync<'a>(
        &self,
        owner: &'a OperationOwner,
    ) -> Result<crate::IdleSession<'a>, OperationError> {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(OperationError::Closed);
        }
        owner.try_lock()
    }

    pub(super) fn register(&self) -> Result<Arc<OperationOwner>, OperationError> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(OperationError::Closed);
        }
        // Retain live and abandoned-failure custody, not an archive of reported
        // or successful work after its lane/waiters have been dropped.
        state
            .owners
            .retain(|owner| Arc::strong_count(owner) > 1 || owner.try_lock().is_err());
        let owner = Arc::new(OperationOwner::default());
        state.owners.push(owner.clone());
        Ok(owner)
    }

    pub(super) async fn shutdown(&self) -> Result<(), String> {
        let owners = {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            state.owners.clone()
        };
        // Join every owner even after a failure. Cancellation of this wait keeps
        // the registry and the actual tasks in custody for the next shutdown.
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

#[cfg(test)]
mod tests;
