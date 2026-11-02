//! Physical Store retirement, independent of the lifetime of Session readers.
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::{
    OperationFence,
    operation::{OperationError, OperationOwner},
};

use super::{Session, SessionError, SessionLock, Store, storage::StorageWork};

pub(super) struct NativeStore {
    // SQLite must close before its exclusive writer lease is released.
    pub(super) store: Mutex<Store>,
    pub(super) _lock: SessionLock,
}

#[derive(Default)]
pub(super) struct RetirementWork {
    state: Mutex<State>,
    owner: Arc<OperationOwner>,
    changed: Notify,
}

#[derive(Default)]
enum State {
    #[default]
    Unmanaged,
    Waiting,
    Queued(Box<NativeStore>),
    Starting,
    Started {
        fence: OperationFence,
        io_errors: Vec<String>,
    },
}

/// Retained custody of physical SQLite close and writer-lease release.
///
/// Acquire this before releasing the Session. Keep it until `join` succeeds,
/// including when the last Session reference belongs to an owned reader future.
/// It does not keep the Session alive. All Session references must be released
/// before joining; shutdown alone deliberately preserves read-only access.
/// Dropping a wait does not discard the actual native task or its failure.
#[derive(Clone)]
#[must_use = "retain retirement custody until all Session references are released and joined"]
pub struct SessionRetirement {
    storage: Arc<StorageWork>,
}

impl Session {
    pub(crate) async fn join_released_child_resources(&self) {
        for retirement in self.storage.children.released() {
            // Historical failures stay retained for the outer resource join.
            let _ = retirement.join().await;
        }
    }

    pub(crate) fn retain_child_resources(&self, child: &Session) -> Result<(), SessionError> {
        self.storage.children.register(child)
    }

    /// Manage destruction off-executor. Bare synchronous Session operators keep
    /// their synchronous destruction contract when no retirement is acquired.
    pub fn retirement(&self) -> SessionRetirement {
        let mut state = self
            .storage
            .retirement
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, State::Unmanaged) {
            *state = State::Waiting;
        }
        drop(state);
        SessionRetirement {
            storage: self.storage.clone(),
        }
    }
}

impl RetirementWork {
    pub(super) fn release(&self, native: NativeStore) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, State::Unmanaged) {
            drop(state);
            drop(native);
            return;
        }
        assert!(matches!(*state, State::Waiting), "Store released once");
        *state = State::Queued(Box::new(native));
        drop(state);
        self.changed.notify_waiters();
    }

    pub(super) fn start(&self, storage: &StorageWork) {
        // A synchronous caller can hand off to a later async retirement join.
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        if !matches!(
            *self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            State::Queued(_)
        ) {
            return;
        }
        let owners = storage.owners();
        let mut io_errors = Vec::new();
        for owner in &owners {
            match owner.try_lock() {
                Ok(_) => {}
                Err(OperationError::Active) => return,
                Err(error) => io_errors.push(error.to_string()),
            }
        }
        let native = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !matches!(*state, State::Queued(_)) {
                return;
            }
            let State::Queued(native) = std::mem::replace(&mut *state, State::Starting) else {
                unreachable!()
            };
            native
        };
        // With no Session references, no new lane can appear. All actual I/O
        // tasks have joined above. Their unread outcomes can themselves own
        // child Sessions or arbitrary destructors: release them on this native
        // leaf, not under the registry lock or while awaiting child retirement.
        let retained = storage.take_owners();
        drop(owners); // `retained` still owns every outcome before native launch.
        let operation = self
            .owner
            .start_blocking(CancellationToken::new(), move || {
                drop(retained);
                drop(native);
                Ok::<_, SessionError>(())
            });
        let fence = operation.fence();
        // Teardown has no cancellable phase. The registry retains the actual
        // blocking task and its outcome; its receipt joins them, not this wait.
        drop(operation);
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            State::Started { fence, io_errors };
        self.changed.notify_waiters();
    }

    fn started(&self) -> bool {
        matches!(
            *self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            State::Started { .. }
        )
    }
}

impl SessionRetirement {
    /// All Session references have gone; native destruction may still be running.
    pub fn is_released(&self) -> bool {
        !matches!(
            *self
                .storage
                .retirement
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            State::Unmanaged | State::Waiting
        )
    }

    pub fn same_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.storage, &other.storage)
    }

    /// Reap only actual completed tasks. An unobserved failure is not discarded.
    pub fn try_join(&self) -> Option<Result<(), SessionError>> {
        self.storage.retirement.start(&self.storage);
        let finished = {
            let state = self
                .storage
                .retirement
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            matches!(&*state, State::Started { fence, .. } if fence.is_finished())
        };
        if !finished {
            return None;
        }
        match self.storage.children.try_join() {
            Some(Ok(())) => {}
            other => return other,
        }
        let owners = self.storage.owners();
        for owner in owners
            .iter()
            .chain(std::iter::once(&self.storage.retirement.owner))
        {
            match owner.try_lock() {
                Ok(_) => {}
                Err(OperationError::Active) => return None,
                Err(error) => return Some(Err(error.into())),
            }
        }
        let state = self
            .storage
            .retirement
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let State::Started { io_errors, .. } = &*state else {
            unreachable!()
        };
        Some(if io_errors.is_empty() {
            Ok(())
        } else {
            Err(SessionError::Storage(io_errors.join("; ")))
        })
    }

    /// Join final release, admitted I/O, and actual native destruction, even
    /// after failure. Cancellation preserves all three in the existing registry.
    pub async fn join(&self) -> Result<(), SessionError> {
        loop {
            let changed = self.storage.retirement.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.is_released() {
                // Do not close read admission while external Session references
                // still exist. After final release, join every lane before
                // moving its unread outcomes into native teardown.
                let _ = self.storage.shutdown().await;
                self.storage.retirement.start(&self.storage);
            }
            if self.storage.retirement.started() {
                break;
            }
            changed.await;
        }
        // Last-reference release makes Store access impossible, but its native
        // worker can still be returning. Join both lifecycles before reporting.
        let io = {
            let state = self
                .storage
                .retirement
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let State::Started { io_errors, .. } = &*state else {
                unreachable!()
            };
            if io_errors.is_empty() {
                Ok(())
            } else {
                Err(io_errors.join("; "))
            }
        };
        let native = self.storage.retirement.owner.join().await;
        let children = Box::pin(self.storage.children.join()).await;
        let native = match (native, children) {
            (Ok(()), result) => result,
            (Err(native), Ok(())) => Err(native.into()),
            (Err(native), Err(children)) => Err(SessionError::Storage(format!(
                "{native}; child resources also failed: {children}"
            ))),
        };
        match (io, native) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(SessionError::Storage(error)),
            (Ok(()), Err(error)) => Err(error),
            (Err(io), Err(native)) => Err(SessionError::Storage(format!(
                "{io}; Store retirement also failed: {native}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests;
