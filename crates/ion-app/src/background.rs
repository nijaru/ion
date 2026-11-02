//! Bounded application ownership of transferred Sessions, independent of readers.
#[cfg(test)]
#[path = "control/background_tests.rs"]
mod tests;
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

use crate::{
    ChildActivitySnapshot, ChildProgress, ChildProgressSnapshot, ChildSteering, ChildSteeringReport,
};
use anyhow::{Result, ensure};
use ion_core::{
    BackgroundJob, BackgroundSlot, BackgroundStartError, DelegationReceipt, Operation, Session,
    SessionError,
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
// so retaining Shared here would create a cycle through its runs.
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
struct ObservationWake(Weak<Shared>);
impl Wake for ObservationWake {
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
    /// Joined sources remain in flight until recipient intake is published.
    offering: usize,
    runs: BTreeMap<uuid::Uuid, Run>,
    failures: Vec<String>,
}
struct Run {
    // Latest confirmed transfer or settlement; joining errors cannot erase it.
    receipt: DelegationReceipt,
    parent: Weak<Session>,
    /// None while synchronously leased for polling, or after result adoption.
    /// `result`, not handle presence, is the authoritative join publication.
    operation: Option<Operation<DelegationReceipt, SessionError>>,
    result: Option<Result<(), SessionError>>,
    /// Render once outside lifecycle custody; inspection only copies this value.
    error: Option<String>,
    steering: Option<ChildSteering>,
    progress: Option<ChildProgress>,
    completion: Option<Arc<ion_core::CompletionPermit>>,
    stop: CancellationToken,
}

/// Cold live destination, not serialized authority.
pub struct BackgroundDestination {
    completion_target: Option<crate::CompletionTarget>,
    completion: Option<Arc<ion_core::CompletionPermit>>,
    steering: Option<ChildSteering>,
    progress: Option<ChildProgress>,
    shared: Arc<Shared>,
    reserved: bool,
    stop: Option<CancellationToken>,
}
#[derive(Clone, Serialize)]
pub struct BackgroundSnapshot {
    pub owner: uuid::Uuid,
    /// Invalidation lower bound, not a cross-owner atomic snapshot coordinate.
    pub revision: u64,
    pub closed: bool,
    pub failed: bool,
    pub reserved: usize,
    pub runs: Vec<BackgroundRunSnapshot>,
}
#[derive(Clone, Serialize)]
pub struct BackgroundJobStatus {
    pub completion: Option<ion_core::CompletionStatus>,
    pub id: uuid::Uuid,
    pub model: ion_ai::ModelRef,
    pub parent: uuid::Uuid,
    pub path: std::path::PathBuf,
    pub joined: bool,
    pub end: Option<ion_core::TurnEndReason>,
    pub error: Option<String>,
    pub progress: Option<ChildActivitySnapshot>,
}
#[derive(Clone, Serialize)]
pub struct BackgroundSummary {
    pub running: usize,
    pub joined: usize,
    pub reserved: usize,
}
#[derive(Clone, Serialize)]
pub struct BackgroundRunSnapshot {
    pub completion: Option<ion_core::CompletionStatus>,
    pub receipt: DelegationReceipt,
    /// Actual retained operation joined, not merely visible run completion.
    pub joined: bool,
    pub error: Option<String>,
    pub steering: Vec<ChildSteeringReport>,
    pub progress: Option<ChildProgressSnapshot>,
}
/// Infrastructure acknowledgement retains the complete view as well as its
/// original typed cause; releasing custody must not discard returned guidance.
pub struct BackgroundRunFailure {
    pub run: BackgroundRunSnapshot,
    pub error: SessionError,
}
impl std::fmt::Debug for BackgroundRunFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::fmt::Display for BackgroundRunFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for BackgroundRunFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Non-consuming selection snapshot after any join or expiry of one wait budget.
/// Expiry and dropping this report do not stop or acknowledge selected runs.
#[derive(Clone, Serialize)]
pub struct BackgroundWait {
    pub wait_expired: bool,
    /// All selected runs, in requested order, including still-running jobs.
    pub runs: Vec<BackgroundRunSnapshot>,
}
struct RunInspection {
    receipt: DelegationReceipt,
    joined: bool,
    error: Option<String>,
    steering: Option<ChildSteering>,
    progress: Option<ChildProgress>,
    completion: Option<Arc<ion_core::CompletionPermit>>,
}
impl Run {
    fn inspect(&self) -> RunInspection {
        RunInspection {
            receipt: self.receipt.clone(),
            joined: self.result.is_some(),
            error: self.error.clone(),
            steering: self.steering.clone(),
            progress: self.progress.clone(),
            completion: self.completion.clone(),
        }
    }
}
impl RunInspection {
    fn snapshot(self) -> BackgroundRunSnapshot {
        BackgroundRunSnapshot {
            completion: self
                .completion
                .as_ref()
                .map(|permit| permit.receipt().snapshot()),
            receipt: self.receipt,
            joined: self.joined,
            progress: self.progress.as_ref().and_then(ChildProgress::snapshot),
            steering: self
                .steering
                .as_ref()
                .map_or_else(Vec::new, ChildSteering::snapshot),
            error: self.error,
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
            completion_target: None,
            completion: None,
            steering: None,
            progress: None,
            shared: self.shared.clone(),
            reserved: false,
            stop: None,
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
            || state.offering != 0
            || state.runs.values().any(|run| run.result.is_none())
    }
    /// Sticky after acknowledgement. Joining enriches an early task-loss signal
    /// with original operation errors; neither a reader nor a tool can clear it.
    pub fn check(&self) -> Result<(), SessionError> {
        if !self.shared.failed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let state = self.shared.lock();
        let message = if state.failures.is_empty() {
            "background infrastructure failed; inspect committed facts and effects".into()
        } else {
            state.failures.join("; ")
        };
        Err(SessionError::DelegationInfrastructure(message))
    }
    /// Privileged whole custody view; applications authorize/project disclosure.
    pub fn snapshot(&self) -> BackgroundSnapshot {
        let state = self.shared.lock();
        // Read revision first: a racing lock-free failure may be included under
        // an older revision, never omitted under an already-consumed new one.
        let revision = self.revision();
        let failed = self.shared.failed.load(Ordering::SeqCst);
        let closed = self.shared.closed.load(Ordering::SeqCst);
        let reserved = state.reserved;
        let runs: Vec<_> = state.runs.values().map(Run::inspect).collect();
        drop(state);
        BackgroundSnapshot {
            owner: self.shared.id,
            revision,
            closed,
            failed,
            reserved,
            runs: runs.into_iter().map(RunInspection::snapshot).collect(),
        }
    }
    /// Job coordinates/status without copying tasks or successful answers.
    pub fn jobs(&self) -> Vec<BackgroundJobStatus> {
        let runs: Vec<_> = self
            .shared
            .lock()
            .runs
            .values()
            .map(|run| {
                let receipt = &run.receipt;
                let (end, error) = match &receipt.outcome {
                    Some(ion_core::DelegationOutcome::Joined { receipt, error, .. }) => {
                        (receipt.as_ref().and_then(|r| r.end.clone()), error.clone())
                    }
                    _ => (None, None),
                };
                (
                    BackgroundJobStatus {
                        completion: None,
                        id: receipt.record.run,
                        model: receipt.record.spec.model.clone(),
                        parent: receipt.record.site.session,
                        path: receipt.record.path.clone(),
                        joined: run.result.is_some(),
                        end,
                        progress: None,
                        error: run.error.clone().or(error),
                    },
                    run.completion.clone(),
                    run.progress.clone(),
                )
            })
            .collect();
        runs.into_iter()
            .map(|(mut job, completion, progress)| {
                job.completion = completion
                    .as_ref()
                    .map(|permit| permit.receipt().snapshot());
                job.progress = progress.as_ref().and_then(ChildProgress::activity);
                job
            })
            .collect()
    }
    /// Cheap presentation counters, without copying tasks or retained answers.
    pub fn summary(&self) -> BackgroundSummary {
        let state = self.shared.lock();
        let running = state
            .runs
            .values()
            .filter(|run| run.result.is_none())
            .count();
        BackgroundSummary {
            running,
            joined: state.runs.len() - running,
            reserved: state.reserved,
        }
    }
    pub fn run(&self, run: uuid::Uuid) -> Option<BackgroundRunSnapshot> {
        let run = self.shared.lock().runs.get(&run).map(Run::inspect);
        run.map(RunInspection::snapshot)
    }
    /// Reuse an actual retained parent while a transferred task holds its writer.
    /// Weak handles do not let unread results pin a writer after settlement.
    pub(crate) fn parent(&self, path: &Path) -> Option<Arc<Session>> {
        let parents: Vec<_> = self
            .shared
            .lock()
            .runs
            .values()
            .map(|run| run.parent.clone())
            .collect();
        parents
            .into_iter()
            .filter_map(|parent| parent.upgrade())
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
                .runs
                .values()
                .any(|run| run.result.is_none());
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
            // Core join polling can itself notify waiters. Lease a bounded
            // batch synchronously, poll outside state, and adopt before any
            // yield. A leased run remains running until its result is retained.
            let operations: Vec<_> = {
                let mut state = self.shared.lock();
                state
                    .runs
                    .iter_mut()
                    .filter_map(|(&id, run)| run.operation.take().map(|operation| (id, operation)))
                    .collect()
            };
            let mut operation_context = Context::from_waker(&self.shared.completion_waker);
            let polled: Vec<_> = operations
                .into_iter()
                .map(|(id, mut operation)| {
                    let result = Pin::new(&mut operation).poll(&mut operation_context);
                    let error = match &result {
                        Poll::Ready(Err(SessionError::DelegationSettledFailure {
                            error, ..
                        })) => Some(error.to_string()),
                        Poll::Ready(Err(error)) => Some(error.to_string()),
                        _ => None,
                    };
                    (id, operation, result, error)
                })
                .collect();
            // Registering/replacing a waiter may destroy a caller's old waker.
            let notified_ready = notified.as_mut().poll(cx).is_ready();
            let mut completed = Vec::new();
            let mut failures = Vec::new();
            let mut offers = Vec::new();
            let mut state = self.shared.lock();
            for (id, operation, result, error) in polled {
                let run = state
                    .runs
                    .get_mut(&id)
                    .expect("leased run cannot be acknowledged");
                match result {
                    Poll::Pending => run.operation = Some(operation),
                    Poll::Ready(result) => {
                        let result = match result {
                            Ok(receipt) => {
                                run.receipt = receipt;
                                Ok(())
                            }
                            Err(SessionError::DelegationSettledFailure { receipt, error }) => {
                                run.receipt = *receipt;
                                Err(*error)
                            }
                            Err(error) => Err(error),
                        };
                        if let Some(error) = &error {
                            failures.push(error.clone());
                        }
                        run.error = error;
                        if result.is_ok()
                            && let Some(permit) = &run.completion
                        {
                            offers.push((
                                permit.clone(),
                                ion_core::CompletionNotice {
                                    source_session: run.receipt.record.site.session,
                                    run: run.receipt.record.run,
                                    child_session: run.receipt.record.child,
                                },
                            ));
                        }
                        run.result = Some(result);
                        completed.push(operation);
                    }
                }
            }
            let busy = state.runs.values().any(|run| run.result.is_none());
            let failed = !failures.is_empty();
            let close_changed = if failed {
                state.failures.extend(failures);
                self.shared.mark_closed(true)
            } else {
                false
            };
            let joined = !completed.is_empty();
            let ready = joined || !busy || notified_ready;
            let open = !self.shared.closed.load(Ordering::SeqCst);
            let offering = offers.len();
            state.offering += offering;
            drop(state);
            // Operation Drop, cancellation and notification can wake arbitrary
            // observers. None may run beneath source lifecycle custody.
            drop(completed);
            if failed {
                self.shared.publish_close(close_changed);
            }
            if joined {
                self.shared.invalidate();
            }
            // Receiver intake cannot run under source custody: receiving
            // admission consults its own application/background owner.
            if open {
                for (permit, notice) in offers {
                    permit.offer(notice);
                }
            }
            if offering != 0 {
                self.shared.lock().offering -= offering;
                self.shared.invalidate();
            }
            if ready {
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
            "wait requires 1..={} selected runs",
            self.shared.capacity
        );
        ensure!(
            ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
            "wait requires unique run IDs"
        );
        let deadline = tokio::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow::anyhow!("wait budget exceeds the clock range"))?;
        loop {
            let revision = self.revision();
            let report = {
                let state = self.shared.lock();
                let runs = ids
                    .iter()
                    .map(|id| {
                        state.runs.get(id).ok_or_else(|| {
                            anyhow::anyhow!("run is not owned here (unknown or acknowledged)")
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let ready = runs.iter().any(|run| run.result.is_some());
                (ready || tokio::time::Instant::now() >= deadline).then(|| {
                    (
                        !ready,
                        runs.into_iter().map(Run::inspect).collect::<Vec<_>>(),
                    )
                })
            };
            self.check()?;
            if let Some((wait_expired, runs)) = report {
                return Ok(BackgroundWait {
                    wait_expired,
                    runs: runs.into_iter().map(RunInspection::snapshot).collect(),
                });
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
        run: uuid::Uuid,
        request: &ion_core::RequestKey,
        input: &ion_ai::Message,
        stop: &CancellationToken,
    ) -> Result<ChildSteeringReport> {
        self.ensure_open()?;
        let (expected, steering) = {
            let state = self.shared.lock();
            let run = state.runs.get(&run).ok_or_else(|| {
                anyhow::anyhow!("run is not owned here (unknown or acknowledged)")
            })?;
            let steering = run
                .steering
                .clone()
                .ok_or_else(|| anyhow::anyhow!("this run has no live steering grant"))?;
            (
                (run.receipt.record.child, run.receipt.record.request_key()),
                steering,
            )
        };
        steering
            .steer_run(Some(expected), request, input, stop)
            .await
    }
    pub fn request_stop(&self, id: uuid::Uuid) -> Result<()> {
        let stop = {
            let state = self.shared.lock();
            let run = state.runs.get(&id).ok_or_else(|| {
                anyhow::anyhow!("run is not owned here (unknown or acknowledged)")
            })?;
            run.result.is_none().then(|| run.stop.clone())
        };
        if let Some(stop) = stop {
            stop.cancel();
        }
        self.shared.invalidate();
        Ok(())
    }
    pub fn request_shutdown(&self) {
        let (changed, permits) = {
            let state = self.shared.lock();
            let changed = self.shared.mark_closed(false);
            let permits: Vec<_> = state
                .runs
                .values()
                .filter_map(|run| run.completion.clone())
                .collect();
            (changed, permits)
        };
        self.shared.publish_close(changed);
        for permit in permits {
            permit.withdraw();
        }
    }
    /// Natural completion without stopping runs. Interrupted waits retain all
    /// operations/results; the application must still explicitly close and join.
    pub async fn drain(&self) -> Result<(), SessionError> {
        loop {
            let revision = self.revision();
            if !self.has_work() {
                return self.check();
            }
            self.changed(revision).await;
        }
    }
    pub async fn join_shutdown(&self) -> Result<(), SessionError> {
        self.request_shutdown();
        self.drain().await
    }
    /// Explicitly acknowledge one joined result. Unknown/unjoined returns None;
    /// Returns the complete final view, including returned guidance, atomically
    /// with releasing custody. Infrastructure failures retain their typed cause.
    pub fn take(
        &self,
        run: uuid::Uuid,
    ) -> Option<Result<BackgroundRunSnapshot, BackgroundRunFailure>> {
        let mut state = self.shared.lock();
        state.runs.get(&run)?.result.as_ref()?;
        let run = state.runs.remove(&run)?;
        drop(state);
        if let Some(permit) = &run.completion {
            permit.acknowledge();
        }
        let completion = run
            .completion
            .as_ref()
            .map(|permit| permit.receipt().snapshot());
        let steering = run
            .steering
            .as_ref()
            .map_or_else(Vec::new, ChildSteering::snapshot);
        let progress = run.progress.as_ref().and_then(ChildProgress::snapshot);
        let result = run.result.expect("joined run retains its result");
        let snapshot = BackgroundRunSnapshot {
            completion,
            receipt: run.receipt,
            joined: true,
            error: result.as_ref().err().map(ToString::to_string),
            steering,
            progress,
        };
        let result = match result {
            Ok(()) => Ok(snapshot),
            Err(error) => Err(BackgroundRunFailure {
                run: snapshot,
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
                // PoisonError itself owns the guard. Release it before waking
                // observers, then reacquire once without recursive recovery.
                self.state.clear_poison();
                drop(error.into_inner());
                self.close(true);
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            }
        }
    }
    fn invalidate(&self) {
        self.revision
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_add(1))
            .expect("background revision exhausted");
        self.changed.notify_waiters();
    }
    // Fence admission under the state lock; publish only after releasing it.
    fn mark_closed(&self, failure: bool) -> bool {
        let closed = self.closed.swap(true, Ordering::SeqCst);
        let failed = failure && !self.failed.swap(true, Ordering::SeqCst);
        !closed || failed
    }
    fn publish_close(&self, changed: bool) {
        self.stop.cancel();
        if changed {
            self.invalidate();
        }
    }
    fn close(&self, failure: bool) {
        let changed = self.mark_closed(failure);
        self.publish_close(changed);
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
    /// Explicit receiving grant; unrelated peers are allowed when authorized.
    pub fn with_completion(mut self, target: crate::CompletionTarget) -> Self {
        self.completion_target = Some(target);
        self
    }
    /// Attach the read-only observation belonging to this prepared child.
    /// Snapshot coordinates identify the actual observed Session/request.
    pub fn with_progress(mut self, progress: ChildProgress) -> Self {
        self.progress = Some(progress);
        self
    }
    /// Attach the matching prepared live grant; actual run identity is checked
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
    fn reserve(&mut self) -> std::result::Result<CancellationToken, String> {
        if self.reserved {
            return Ok(self.stop.as_ref().expect("reserved stop scope").clone());
        }
        tokio::runtime::Handle::try_current().map_err(|error| error.to_string())?;
        // Receiving custody may notify or consult this same background owner.
        // A refused cold permit is also destroyed only after state unlocks.
        let completion = self
            .completion_target
            .as_ref()
            .map(|target| target.reserve().map(Arc::new))
            .transpose()
            .map_err(|error| error.to_string())?;
        let mut state = self.shared.lock();
        let refusal = if self.shared.closed.load(Ordering::SeqCst) {
            Some("background owner is closed")
        } else if state.reserved + state.runs.len() == self.shared.capacity {
            Some("background custody is full")
        } else {
            None
        };
        if let Some(refusal) = refusal {
            drop(state);
            drop(completion);
            return Err(refusal.into());
        }
        state.reserved += 1;
        drop(state);
        self.completion = completion;
        self.reserved = true;
        let stop = self.shared.stop.child_token();
        self.stop = Some(stop.clone());
        self.shared.invalidate();
        Ok(stop)
    }
    fn install<'a>(
        mut self: Box<Self>,
        job: &'a mut BackgroundJob,
    ) -> ion_ai::BoxFuture<'a, std::result::Result<DelegationReceipt, BackgroundStartError>> {
        Box::pin(async move {
            {
                let _state = self.shared.lock();
                if !self.reserved {
                    return Err(BackgroundStartError::Infrastructure(
                        SessionError::InvalidHistory,
                    ));
                }
                if self.shared.closed.load(Ordering::SeqCst) {
                    return Err(BackgroundStartError::Refused(
                        "background owner is closed".into(),
                    ));
                }
            }
            if let Some(steering) = &self.steering {
                steering.observe_changes(Waker::from(Arc::new(ObservationWake(Arc::downgrade(
                    &self.shared,
                )))));
            }
            if let Some(progress) = &self.progress {
                progress.observe_changes(Waker::from(Arc::new(ObservationWake(Arc::downgrade(
                    &self.shared,
                )))));
            }
            let parent = Arc::downgrade(job.parent());
            let weak = Arc::downgrade(&self.shared);
            // Keep this destination and its reservation across the actual transfer,
            // but never its lifecycle lock. A racing close may lose to commit: adopt
            // that operation even when closed, so shutdown can join it reliably.
            let (receipt, operation) = job
                .start(self.shared.id, move || {
                    if let Some(shared) = weak.upgrade() {
                        shared.close(true);
                    }
                })
                .await?;
            let mut state = self.shared.lock();
            let completion = self.completion.clone();
            let replaced = state.runs.insert(
                receipt.record.run,
                Run {
                    receipt: receipt.clone(),
                    parent,
                    operation: Some(operation),
                    result: None,
                    error: None,
                    steering: self.steering.take(),
                    progress: self.progress.take(),
                    completion: self.completion.take(),
                    stop: self.stop.take().expect("installed reserved stop scope"),
                },
            );
            state.reserved -= 1;
            self.reserved = false;
            let closed = self.shared.closed.load(Ordering::SeqCst);
            drop(state);
            drop(replaced);
            if closed && let Some(permit) = completion {
                permit.withdraw();
            }
            self.shared.invalidate();
            Ok(receipt)
        })
    }
}
