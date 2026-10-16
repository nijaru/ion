//! Bounded live ownership of transferred Sessions, independent of parent waits.
use std::{
    collections::BTreeMap,
    future::poll_fn,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
};

use anyhow::{Result, ensure};
use ion_core::{
    BackgroundJob, BackgroundSlot, BackgroundStartError, CodingSessionError, DelegationReceipt,
    Operation,
};
use serde::Serialize;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// One service/application owner epoch. Reserve before delegation admission;
/// unread joined results still occupy capacity. There is no task scheduler or
/// second agent loop here. Drop closes/stops; explicit shutdown joins. Neither
/// runtime destruction nor a panicked child promises physical effect quiescence.
pub struct BackgroundOwner {
    shared: Arc<Shared>,
}
struct Shared {
    id: uuid::Uuid,
    capacity: usize,
    state: Mutex<State>,
    closed: AtomicBool,
    failed: AtomicBool,
    stop: CancellationToken,
    changed: Notify,
}
#[derive(Default)]
struct State {
    reserved: usize,
    children: BTreeMap<uuid::Uuid, Child>,
}
struct Child {
    receipt: DelegationReceipt,
    operation: Option<Operation<DelegationReceipt, CodingSessionError>>,
    result: Option<Result<DelegationReceipt, CodingSessionError>>,
}

/// A cold live destination. Core reserves custody only after reconciling the
/// initiating occurrence and before committing intent. Not serialized authority.
pub struct BackgroundDestination {
    shared: Arc<Shared>,
    reserved: bool,
}

#[derive(Clone, Serialize)]
pub struct BackgroundSnapshot {
    pub owner: uuid::Uuid,
    pub closed: bool,
    pub failed: bool,
    pub reserved: usize,
    pub children: Vec<BackgroundChildSnapshot>,
}
#[derive(Clone, Serialize)]
pub struct BackgroundChildSnapshot {
    pub receipt: DelegationReceipt,
    /// Actual retained operation joined, not merely visible child completion.
    pub joined: bool,
    pub error: Option<String>,
}

impl BackgroundOwner {
    /// Bounds retained jobs, reservations and unread results together (1..=64).
    /// This is not a history, encoded-byte or process RSS quota.
    pub fn new(capacity: usize) -> Result<Self> {
        ensure!(
            (1..=64).contains(&capacity),
            "background capacity must be 1..=64"
        );
        Ok(Self {
            shared: Arc::new(Shared {
                id: uuid::Uuid::new_v4(),
                capacity,
                state: Mutex::new(State::default()),
                closed: AtomicBool::new(false),
                failed: AtomicBool::new(false),
                stop: CancellationToken::new(),
                changed: Notify::new(),
            }),
        })
    }

    pub fn destination(&self) -> BackgroundDestination {
        BackgroundDestination {
            shared: self.shared.clone(),
            reserved: false,
        }
    }

    /// Privileged whole custody view. Does not consume or drive results and
    /// confers no execution authority. Applications authorize/project disclosure.
    pub fn snapshot(&self) -> BackgroundSnapshot {
        let state = self.shared.lock();
        // Read failed first: failed implies closed, even when the lock-free
        // failure hook races this view during task destruction.
        let failed = self.shared.failed.load(Ordering::SeqCst);
        let closed = self.shared.closed.load(Ordering::SeqCst);
        BackgroundSnapshot {
            owner: self.shared.id,
            closed,
            failed,
            reserved: state.reserved,
            children: state
                .children
                .values()
                .map(|child| BackgroundChildSnapshot {
                    receipt: child
                        .result
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .unwrap_or(&child.receipt)
                        .clone(),
                    joined: child.operation.is_none(),
                    error: child
                        .result
                        .as_ref()
                        .and_then(|result| result.as_ref().err())
                        .map(ToString::to_string),
                })
                .collect(),
        }
    }

    /// Drive one or more joins without transferring result custody. Cancelling
    /// this wait retains operations and their original results. No output reader
    /// is involved in stop, infrastructure closure, or child execution.
    pub async fn maintain(&self) {
        let notified = self.shared.changed.notified();
        tokio::pin!(notified);
        poll_fn(|cx| {
            let mut state = self.shared.lock();
            let mut completed = false;
            let mut busy = false;
            for child in state.children.values_mut() {
                if let Some(operation) = &mut child.operation {
                    match Pin::new(operation).poll(cx) {
                        Poll::Pending => busy = true,
                        Poll::Ready(result) => {
                            if result.is_err() {
                                self.shared.close(true);
                            }
                            child.operation = None;
                            child.result = Some(result);
                            completed = true;
                        }
                    }
                }
            }
            if completed {
                // An Operation has one notification future. Several callers
                // may poll it with different wakers, so publish the joined
                // custody change to every owner waiter, not just its last poller.
                self.shared.changed.notify_waiters();
            }
            if completed || !busy || notified.as_mut().poll(cx).is_ready() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }

    /// Stop only the transferred child named by this owner, not its parent.
    pub fn request_stop(&self, child: uuid::Uuid) -> Result<()> {
        let state = self.shared.lock();
        let child = state
            .children
            .get(&child)
            .ok_or_else(|| anyhow::anyhow!("child is not owned here"))?;
        if let Some(operation) = &child.operation {
            operation.request_stop();
        }
        Ok(())
    }

    /// Close before requesting stop. Parent stop/shutdown does not cross this
    /// ownership boundary; owner shutdown stops all its transferred children.
    pub fn request_shutdown(&self) {
        // Serialize explicit shutdown with installation. Failure closure must
        // also work from a task dropped synchronously by spawn, so it uses
        // atomic flags and cancellation instead of reacquiring this lock.
        let _state = self.shared.lock();
        self.shared.close(false);
    }

    pub async fn join_shutdown(&self) {
        self.request_shutdown();
        loop {
            if self
                .shared
                .lock()
                .children
                .values()
                .all(|child| child.operation.is_none())
            {
                return;
            }
            self.maintain().await;
        }
    }

    /// The only operation that releases a joined result's capacity. Unjoined or
    /// unknown IDs return None. Snapshot and shutdown never acknowledge results.
    pub fn take(&self, child: uuid::Uuid) -> Option<Result<DelegationReceipt, CodingSessionError>> {
        let mut state = self.shared.lock();
        if state.children.get(&child)?.operation.is_some() {
            return None;
        }
        state.children.remove(&child)?.result
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                // Preserve join custody after panic, but never reuse admission.
                self.close(true);
                error.into_inner()
            }
        }
    }
    fn close(&self, failure: bool) {
        self.closed.store(true, Ordering::SeqCst);
        if failure {
            self.failed.store(true, Ordering::SeqCst);
        }
        self.stop.cancel();
        self.changed.notify_waiters();
    }
}
impl Drop for BackgroundOwner {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}
impl Drop for BackgroundDestination {
    fn drop(&mut self) {
        if self.reserved {
            self.shared.lock().reserved -= 1;
        }
    }
}
impl BackgroundSlot for BackgroundDestination {
    fn owner(&self) -> uuid::Uuid {
        self.shared.id
    }
    fn reserve(&mut self) -> std::result::Result<(), String> {
        if self.reserved {
            return Ok(());
        }
        tokio::runtime::Handle::try_current().map_err(|error| error.to_string())?;
        let mut state = self.shared.lock();
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err("background owner is closed".into());
        }
        if state.reserved + state.children.len() == self.shared.capacity {
            return Err("background custody is full".into());
        }
        state.reserved += 1;
        self.reserved = true;
        Ok(())
    }
    fn install(
        mut self: Box<Self>,
        job: BackgroundJob,
    ) -> std::result::Result<DelegationReceipt, BackgroundStartError> {
        let mut state = self.shared.lock();
        if !self.reserved {
            return Err(BackgroundStartError::Infrastructure(
                CodingSessionError::InvalidHistory,
            ));
        }
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(BackgroundStartError::Refused(
                "background owner is closed".into(),
            ));
        }
        let weak = Arc::downgrade(&self.shared);
        let (receipt, operation) =
            job.start(self.shared.id, self.shared.stop.child_token(), move || {
                if let Some(shared) = weak.upgrade() {
                    shared.close(true);
                }
            })?;
        state.children.insert(
            receipt.record.child,
            Child {
                receipt: receipt.clone(),
                operation: Some(operation),
                result: None,
            },
        );
        state.reserved -= 1;
        self.reserved = false;
        self.shared.changed.notify_waiters();
        Ok(receipt)
    }
}
