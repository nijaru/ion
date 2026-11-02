//! Session-owned custody of blocking storage work. This is neither another
//! writer nor a durable work queue: all lanes use the existing Session Store.
use std::sync::{Arc, Mutex};

use crate::operation::{OperationError, OperationFailure, OperationOwner};

use super::{RequestKey, RequestReceipt, Session, SessionError, SessionStatus, SessionView};

/// A serial lane for blocking Session operations on Tokio's blocking pool.
///
/// Each lane retains one actual operation at a time. Overlapping calls, including
/// reuse while an abandoned call is still running, fail with `OperationActive`;
/// create a separate lane for independent concurrent tasks. Dropping a wait does
/// not cancel its storage work. Unread failures prevent reuse and are reported by
/// Session shutdown, which joins the actual blocking tasks before returning.
///
/// The lane keeps the Session writer lease alive. Session shutdown closes both
/// existing lanes and the creation of new lanes. Use `SessionReader` for explicit
/// read-only access after shutdown.
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
            .run(&self.owner, Access::ReadWrite, move || work(&session))
            .await
    }
}

/// A serial, read-only lane on the Session's existing Store and blocking pool.
///
/// Reads remain available after completed shutdown, including failed shutdown.
/// They are rejected while shutdown is joining work. Dropping a read wait does
/// not release its work or writer lease; the next shutdown joins it. Overlapping
/// calls and unread failures have the same custody rules as `SessionIo`.
pub struct SessionReader {
    session: Arc<Session>,
    owner: Arc<OperationOwner>,
}

impl SessionReader {
    pub(super) fn new(session: &Arc<Session>) -> Result<Self, SessionError> {
        Ok(Self {
            session: session.clone(),
            owner: session.storage.register_access(Access::ReadOnly)?,
        })
    }

    pub async fn view(&self) -> Result<SessionView, SessionError> {
        self.run(Session::view).await
    }

    pub async fn status(&self) -> Result<SessionStatus, SessionError> {
        self.run(Session::status).await
    }

    pub async fn request_receipt(
        &self,
        key: &RequestKey,
    ) -> Result<Option<RequestReceipt>, SessionError> {
        let key = key.clone();
        self.run(move |session| session.request_receipt(&key)).await
    }

    pub async fn reconcile_request(
        &self,
        key: &RequestKey,
        input: &ion_ai::Message,
    ) -> Result<Option<RequestReceipt>, SessionError> {
        let key = key.clone();
        let input = input.clone();
        self.run(move |session| session.reconcile_request(&key, &input))
            .await
    }

    // Deliberately private: public callers cannot smuggle a write through this
    // lane after write-capable admission has closed.
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Session) -> Result<T, SessionError> + Send + 'static,
    ) -> Result<T, SessionError> {
        let session = self.session.clone();
        self.session
            .storage
            .run(&self.owner, Access::ReadOnly, move || work(&session))
            .await
    }
}

#[derive(Default)]
pub(super) struct StorageWork {
    state: Mutex<State>,
    shutdown: tokio::sync::Mutex<()>,
    admissions_changed: tokio::sync::Notify,
}

#[derive(Clone, Copy)]
pub(super) enum Access {
    ReadWrite,
    ReadOnly,
}

#[derive(Default)]
enum Phase {
    #[default]
    Open,
    Closing,
    Closed,
}

impl Phase {
    fn admits(&self, access: Access) -> bool {
        matches!(self, Self::Open) || matches!((self, access), (Self::Closed, Access::ReadOnly))
    }
}

#[derive(Default)]
struct State {
    phase: Phase,
    admissions: usize,
    owners: Vec<Arc<OperationOwner>>,
}

// Reserve admission under the registry lock, then start work outside it. Owner
// startup can notify waiters or drop caller-owned captures on refusal. Shutdown
// waits for these short reservations before taking its stable owner snapshot.
struct Admission<'a>(&'a StorageWork);

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        {
            let mut state = self
                .0
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.admissions -= 1;
        }
        self.0.admissions_changed.notify_one();
    }
}

impl StorageWork {
    pub(super) async fn run<T, E>(
        &self,
        owner: &Arc<OperationOwner>,
        access: Access,
        work: impl FnOnce() -> Result<T, E> + Send + 'static,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: Send + OperationFailure + From<OperationError> + 'static,
    {
        let admission = self.admit(access)?;
        let operation = owner.start_blocking(tokio_util::sync::CancellationToken::new(), work);
        drop(admission);
        operation.await
    }

    pub(super) fn reserve_sync<'a>(
        &self,
        owner: &'a OperationOwner,
    ) -> Result<crate::IdleSession<'a>, OperationError> {
        let _admission = self.admit(Access::ReadWrite)?;
        owner.try_lock()
    }

    fn admit(&self, access: Access) -> Result<Admission<'_>, OperationError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.phase.admits(access) {
            return Err(OperationError::Closed);
        }
        state.admissions += 1;
        Ok(Admission(self))
    }

    pub(super) fn register(&self) -> Result<Arc<OperationOwner>, OperationError> {
        self.register_access(Access::ReadWrite)
    }

    fn register_access(&self, access: Access) -> Result<Arc<OperationOwner>, OperationError> {
        self.prune();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.phase.admits(access) {
            return Err(OperationError::Closed);
        }
        let owner = Arc::new(OperationOwner::default());
        state.owners.push(owner.clone());
        Ok(owner)
    }

    fn prune(&self) {
        let owners = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owners
            .clone();
        // Owner polling, notifications and destruction of retained caller
        // outcomes must happen outside the registry lock. Only remove owners
        // held solely by the registry and this snapshot, with no unread failure.
        let idle: Vec<_> = owners
            .iter()
            .filter(|owner| Arc::strong_count(owner) == 2 && owner.try_lock().is_ok())
            .collect();
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.owners.retain(|owner| {
                Arc::strong_count(owner) != 2
                    || !idle.iter().any(|candidate| Arc::ptr_eq(owner, candidate))
            });
        }
        // `owners` still retains every removed owner until after unlocking.
    }

    pub(super) async fn shutdown(&self) -> Result<(), String> {
        let _shutdown = self.shutdown.lock().await;
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .phase = Phase::Closing;
        let owners = loop {
            let changed = self.admissions_changed.notified();
            let owners = {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (state.admissions == 0).then(|| state.owners.clone())
            };
            if let Some(owners) = owners {
                break owners;
            }
            changed.await;
        };
        // The registry permanently closes write admission; owners themselves
        // stay reusable for explicit readers after this join. Join every owner
        // even after a failure. Cancellation retains Closing and task custody.
        let mut errors = Vec::new();
        for owner in owners {
            if let Err(error) = owner.join().await {
                errors.push(error.to_string());
            }
        }
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .phase = Phase::Closed;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

#[cfg(test)]
mod reader_tests;
#[cfg(test)]
mod tests;
