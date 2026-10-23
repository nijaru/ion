//! Bounded application ownership of transferred Sessions, independent of readers.
use std::{
    collections::{BTreeMap, BTreeSet},
    future::poll_fn,
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    time::Duration,
};

use crate::{ChildSteering, ChildSteeringReport};
use anyhow::{Result, ensure};
use ion_core::{
    BackgroundJob, BackgroundSlot, BackgroundStartError, CodingSession, CodingSessionError,
    DelegationReceipt, Operation,
};
use serde::Serialize;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// One application/service epoch. Reserve before effects; unread joined results
/// occupy capacity. Drop requests stop; explicit shutdown joins. No daemon,
/// scheduler, second agent loop or physical descendant-quiescence guarantee.
pub struct BackgroundOwner {
    shared: Arc<Shared>,
}
struct Shared {
    id: uuid::Uuid,
    capacity: usize,
    state: Mutex<State>,
    closed: AtomicBool,
    failed: AtomicBool,
    revision: AtomicU64,
    stop: CancellationToken,
    changed: Arc<Notify>,
    completion_waker: Waker,
}

// Retain only the notification source, not Shared: Operations store this waker,
// so retaining Shared here would create a cycle through its children.
struct CompletionWake(Arc<Notify>);
impl Wake for CompletionWake {
    fn wake(self: Arc<Self>) {
        self.0.notify_waiters();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify_waiters();
    }
}

// A weak invalidator cannot form a cycle through job control and its receipts.
struct SteeringWake(Weak<Shared>);
impl Wake for SteeringWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(shared) = self.0.upgrade() {
            shared.invalidate();
        }
    }
}

#[derive(Default)]
struct State {
    reserved: usize,
    children: BTreeMap<uuid::Uuid, Child>,
    failures: Vec<String>,
}
struct Child {
    receipt: DelegationReceipt,
    parent: Weak<CodingSession>,
    operation: Option<Operation<DelegationReceipt, CodingSessionError>>,
    result: Option<Result<DelegationReceipt, CodingSessionError>>,
    steering: Option<ChildSteering>,
}

/// Cold live destination, not serialized authority.
pub struct BackgroundDestination {
    steering: Option<ChildSteering>,
    shared: Arc<Shared>,
    reserved: bool,
}
#[derive(Clone, Serialize)]
pub struct BackgroundSnapshot {
    pub owner: uuid::Uuid,
    /// Invalidation lower bound, not a cross-owner atomic snapshot coordinate.
    pub revision: u64,
    pub closed: bool,
    pub failed: bool,
    pub reserved: usize,
    pub children: Vec<BackgroundChildSnapshot>,
}
#[derive(Clone, Serialize)]
pub struct BackgroundJobStatus {
    pub child: uuid::Uuid,
    pub parent: uuid::Uuid,
    pub path: std::path::PathBuf,
    pub joined: bool,
    pub end: Option<ion_core::TurnEndReason>,
    pub error: Option<String>,
}
#[derive(Clone, Serialize)]
pub struct BackgroundSummary {
    pub running: usize,
    pub joined: usize,
    pub reserved: usize,
}
#[derive(Clone, Serialize)]
pub struct BackgroundChildSnapshot {
    pub receipt: DelegationReceipt,
    /// Actual retained operation joined, not merely visible child completion.
    pub joined: bool,
    pub error: Option<String>,
    pub steering: Vec<ChildSteeringReport>,
}
/// Infrastructure acknowledgement retains the complete view as well as its
/// original typed cause; releasing custody must not discard returned guidance.
pub struct BackgroundChildFailure {
    pub child: BackgroundChildSnapshot,
    pub error: CodingSessionError,
}
impl std::fmt::Debug for BackgroundChildFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::fmt::Display for BackgroundChildFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for BackgroundChildFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Non-consuming selection snapshot after any join or expiry of one wait budget.
/// Expiry and dropping this report do not stop or acknowledge selected children.
#[derive(Clone, Serialize)]
pub struct BackgroundWait {
    pub wait_expired: bool,
    /// All selected children, in requested order, including still-running jobs.
    pub children: Vec<BackgroundChildSnapshot>,
}
impl Child {
    fn snapshot(&self) -> BackgroundChildSnapshot {
        BackgroundChildSnapshot {
            receipt: self
                .result
                .as_ref()
                .and_then(|r| r.as_ref().ok())
                .unwrap_or(&self.receipt)
                .clone(),
            joined: self.operation.is_none(),
            steering: self
                .steering
                .as_ref()
                .map_or_else(Vec::new, ChildSteering::snapshot),
            error: self
                .result
                .as_ref()
                .and_then(|r| r.as_ref().err())
                .map(ToString::to_string),
        }
    }
}
impl BackgroundOwner {
    /// Count bounds reservations, jobs and unread results together (1..=64), not RSS.
    pub fn new(capacity: usize) -> Result<Self> {
        ensure!(
            (1..=64).contains(&capacity),
            "background capacity must be 1..=64"
        );
        let changed = Arc::new(Notify::new());
        Ok(Self {
            shared: Arc::new(Shared {
                id: uuid::Uuid::new_v4(),
                capacity,
                state: Mutex::new(State::default()),
                closed: AtomicBool::new(false),
                failed: AtomicBool::new(false),
                revision: AtomicU64::new(0),
                stop: CancellationToken::new(),
                completion_waker: Waker::from(Arc::new(CompletionWake(changed.clone()))),
                changed,
            }),
        })
    }
    pub fn destination(&self) -> BackgroundDestination {
        BackgroundDestination {
            steering: None,
            shared: self.shared.clone(),
            reserved: false,
        }
    }
    pub(crate) fn work_stop(&self) -> CancellationToken {
        self.shared.stop.child_token()
    }
    pub(crate) fn ensure_open(&self) -> Result<()> {
        self.check()?;
        ensure!(
            !self.shared.closed.load(Ordering::SeqCst),
            "application background owner is closed"
        );
        Ok(())
    }
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::SeqCst)
    }
    pub fn has_work(&self) -> bool {
        let state = self.shared.lock();
        state.reserved != 0
            || state
                .children
                .values()
                .any(|child| child.operation.is_some())
    }
    /// Sticky after acknowledgement. Joining enriches an early task-loss signal
    /// with original operation errors; neither a reader nor a tool can clear it.
    pub fn check(&self) -> Result<(), CodingSessionError> {
        if !self.shared.failed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let state = self.shared.lock();
        let message = if state.failures.is_empty() {
            "background infrastructure failed; inspect committed facts and effects".into()
        } else {
            state.failures.join("; ")
        };
        Err(CodingSessionError::DelegationInfrastructure(message))
    }
    /// Privileged whole custody view; applications authorize/project disclosure.
    pub fn snapshot(&self) -> BackgroundSnapshot {
        let state = self.shared.lock();
        // Read revision first: a racing lock-free failure may be included under
        // an older revision, never omitted under an already-consumed new one.
        let revision = self.revision();
        let failed = self.shared.failed.load(Ordering::SeqCst);
        let closed = self.shared.closed.load(Ordering::SeqCst);
        BackgroundSnapshot {
            owner: self.shared.id,
            revision,
            closed,
            failed,
            reserved: state.reserved,
            children: state.children.values().map(Child::snapshot).collect(),
        }
    }
    /// Job coordinates/status without copying tasks or successful answers.
    pub fn jobs(&self) -> Vec<BackgroundJobStatus> {
        self.shared
            .lock()
            .children
            .values()
            .map(|child| {
                let receipt = child
                    .result
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .unwrap_or(&child.receipt);
                let (end, error) = match &receipt.outcome {
                    Some(ion_core::DelegationOutcome::Joined { receipt, error, .. }) => {
                        (receipt.as_ref().and_then(|r| r.end.clone()), error.clone())
                    }
                    _ => (None, None),
                };
                BackgroundJobStatus {
                    child: receipt.record.child,
                    parent: receipt.record.site.session,
                    path: receipt.record.path.clone(),
                    joined: child.operation.is_none(),
                    end,
                    error: child
                        .result
                        .as_ref()
                        .and_then(|r| r.as_ref().err())
                        .map(ToString::to_string)
                        .or(error),
                }
            })
            .collect()
    }
    /// Cheap presentation counters, without copying tasks or retained answers.
    pub fn summary(&self) -> BackgroundSummary {
        let state = self.shared.lock();
        let running = state
            .children
            .values()
            .filter(|child| child.operation.is_some())
            .count();
        BackgroundSummary {
            running,
            joined: state.children.len() - running,
            reserved: state.reserved,
        }
    }
    pub fn child(&self, child: uuid::Uuid) -> Option<BackgroundChildSnapshot> {
        self.shared.lock().children.get(&child).map(Child::snapshot)
    }
    /// Reuse an actual retained parent while a transferred task holds its writer.
    /// Weak handles do not let unread results pin a writer after settlement.
    pub(crate) fn parent(&self, path: &Path) -> Option<Arc<CodingSession>> {
        self.shared
            .lock()
            .children
            .values()
            .filter_map(|child| child.parent.upgrade())
            .find(|parent| parent.path() == path)
    }
    /// Cancellation-safe invalidation, including when every parent is idle.
    /// Results remain in custody; this never acknowledges them. No busy polling
    /// when only unread joined results (or no jobs) remain.
    pub async fn changed(&self, previous: u64) -> u64 {
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let revision = self.revision();
            if revision != previous {
                return revision;
            }
            let running = self
                .shared
                .lock()
                .children
                .values()
                .any(|child| child.operation.is_some());
            if running {
                tokio::select! { () = &mut notified => {}, () = self.maintain() => {} }
            } else {
                notified.await;
            }
        }
    }
    /// Drive joins, retaining original outcomes before yielding. Operations wake
    /// the owner's notification source, never an individual caller. Each borrowed
    /// waiter registers independently, so cancelling one cannot strand the rest.
    pub async fn maintain(&self) {
        let notified = self.shared.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        poll_fn(|cx| {
            let mut operation_context = Context::from_waker(&self.shared.completion_waker);
            let mut state = self.shared.lock();
            let mut completed = false;
            let mut busy = false;
            let mut failures = Vec::new();
            for child in state.children.values_mut() {
                if let Some(operation) = &mut child.operation {
                    match Pin::new(operation).poll(&mut operation_context) {
                        Poll::Pending => busy = true,
                        Poll::Ready(result) => {
                            if let Err(error) = &result {
                                failures.push(error.to_string());
                            }
                            child.operation = None;
                            child.result = Some(result);
                            completed = true;
                        }
                    }
                }
            }
            if !failures.is_empty() {
                state.failures.extend(failures);
                self.shared.close(true);
            }
            if completed {
                self.shared.invalidate();
            }
            if completed || !busy || notified.as_mut().poll(cx).is_ready() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }
    /// Borrowed wait for any selected actual join under one shared budget.
    /// Resolve the entire selection before returning even an already-ready result.
    /// Cancellation or expiry leaves independent operations and reports in custody.
    /// Zero duration polls retained joins; callers choose their own budget policy.
    pub async fn wait(&self, ids: &[uuid::Uuid], timeout: Duration) -> Result<BackgroundWait> {
        ensure!(
            !ids.is_empty() && ids.len() <= self.shared.capacity,
            "wait requires 1..={} selected children",
            self.shared.capacity
        );
        ensure!(
            ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
            "wait requires unique child IDs"
        );
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow::anyhow!("wait budget exceeds the clock range"))?;
        loop {
            let revision = self.revision();
            let report = {
                let state = self.shared.lock();
                let children = ids
                    .iter()
                    .map(|id| {
                        state.children.get(id).ok_or_else(|| {
                            anyhow::anyhow!("child is not owned here (unknown or acknowledged)")
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let ready = children.iter().any(|child| child.operation.is_none());
                (ready || tokio::time::Instant::now() >= deadline).then(|| BackgroundWait {
                    wait_expired: !ready,
                    children: children.into_iter().map(Child::snapshot).collect(),
                })
            };
            self.check()?;
            if let Some(report) = report {
                return Ok(report);
            }
            tokio::select! {
                _ = self.changed(revision) => {},
                () = tokio::time::sleep_until(deadline) => {},
            }
        }
    }
    /// Queue guidance through the live grant captured with this actual job.
    /// A joined job can reconcile a prior key, never admit another input/Turn.
    pub async fn steer(
        &self,
        child: uuid::Uuid,
        request: &ion_core::RequestKey,
        input: &ion_ai::Message,
        stop: &CancellationToken,
    ) -> Result<ChildSteeringReport> {
        self.ensure_open()?;
        let steering = self
            .shared
            .lock()
            .children
            .get(&child)
            .ok_or_else(|| anyhow::anyhow!("child is not owned here (unknown or acknowledged)"))?
            .steering
            .clone()
            .ok_or_else(|| anyhow::anyhow!("this child has no live steering grant"))?;
        steering
            .steer_child(Some(child), request, input, stop)
            .await
    }
    pub fn request_stop(&self, id: uuid::Uuid) -> Result<()> {
        let state = self.shared.lock();
        let child = state
            .children
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("child is not owned here (unknown or acknowledged)"))?;
        if let Some(operation) = &child.operation {
            operation.request_stop();
        }
        self.shared.invalidate();
        Ok(())
    }
    pub fn request_shutdown(&self) {
        let _state = self.shared.lock();
        self.shared.close(false);
    }
    /// Natural completion without stopping children. Interrupted waits retain all
    /// operations/results; the application must still explicitly close and join.
    pub async fn drain(&self) -> Result<(), CodingSessionError> {
        loop {
            let revision = self.revision();
            if !self.has_work() {
                return self.check();
            }
            self.changed(revision).await;
        }
    }
    pub async fn join_shutdown(&self) -> Result<(), CodingSessionError> {
        self.request_shutdown();
        self.drain().await
    }
    /// Explicitly acknowledge one joined result. Unknown/unjoined returns None;
    /// Returns the complete final view, including returned guidance, atomically
    /// with releasing custody. Infrastructure failures retain their typed cause.
    pub fn take(
        &self,
        child: uuid::Uuid,
    ) -> Option<Result<BackgroundChildSnapshot, BackgroundChildFailure>> {
        let mut state = self.shared.lock();
        if state.children.get(&child)?.operation.is_some() {
            return None;
        }
        let child = state.children.remove(&child)?;
        let steering = child
            .steering
            .as_ref()
            .map_or_else(Vec::new, ChildSteering::snapshot);
        let result = match child.result.expect("joined child retains its result") {
            Ok(receipt) => Ok(BackgroundChildSnapshot {
                receipt,
                joined: true,
                error: None,
                steering,
            }),
            Err(error) => Err(BackgroundChildFailure {
                child: BackgroundChildSnapshot {
                    receipt: child.receipt,
                    joined: true,
                    error: Some(error.to_string()),
                    steering,
                },
                error,
            }),
        };
        self.shared.invalidate();
        Some(result)
    }
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                self.close(true);
                error.into_inner()
            }
        }
    }
    fn invalidate(&self) {
        self.revision
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_add(1))
            .expect("background revision exhausted");
        self.changed.notify_waiters();
    }
    fn close(&self, failure: bool) {
        let closed = self.closed.swap(true, Ordering::SeqCst);
        let failed = failure && !self.failed.swap(true, Ordering::SeqCst);
        self.stop.cancel();
        if !closed || failed {
            self.invalidate();
        }
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
            self.shared.invalidate();
        }
    }
}
impl BackgroundDestination {
    /// Attach the matching prepared live grant; actual child identity is checked
    /// again before steering. Serializable job coordinates alone grant no control.
    pub fn with_steering(mut self, steering: ChildSteering) -> Self {
        self.steering = Some(steering);
        self
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
        self.shared.invalidate();
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
        if let Some(steering) = &self.steering {
            steering.observe_changes(Waker::from(Arc::new(SteeringWake(Arc::downgrade(
                &self.shared,
            )))));
        }
        let parent = Arc::downgrade(job.parent());
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
                parent,
                operation: Some(operation),
                result: None,
                steering: self.steering.take(),
            },
        );
        state.reserved -= 1;
        self.reserved = false;
        self.shared.invalidate();
        Ok(receipt)
    }
}
