//! Exclusive, retained custody of one Session operation and its actual task.
use futures_util::FutureExt;
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};
use thiserror::Error;
use tokio::{
    sync::{Notify, futures::OwnedNotified},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum OperationError {
    #[error("a Session operation is active")]
    Active,
    #[error("Session execution is closed")]
    Closed,
    #[error("operation task failed; local effects may remain unknown: {0}")]
    TaskFailed(String),
    #[error("previous operation failed before its completion was observed: {0}")]
    UnreportedFailure(String),
    #[error("owned operations require an active Tokio runtime")]
    RuntimeUnavailable,
    #[error("operation completion has already been consumed")]
    CompletionConsumed,
}

pub(crate) trait OperationFailure: std::fmt::Display {
    fn expected_cancellation(&self) -> bool;
}

struct Completion<T, E>(Mutex<Outcome<T, E>>);
struct Outcome<T, E> {
    result: Option<Result<T, E>>,
    unreported_failure: Option<String>,
}
trait CompletionCustody: Send + Sync {
    fn unreported_failure(&self) -> Option<String>;
}
impl<T: Send, E: Send> CompletionCustody for Completion<T, E> {
    fn unreported_failure(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unreported_failure
            .clone()
    }
}

impl<T, E: OperationFailure> Completion<T, E> {
    fn finish(&self, result: Result<T, E>) {
        let unreported_failure = result
            .as_ref()
            .err()
            .filter(|error| !error.expected_cancellation())
            .map(ToString::to_string);
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Outcome {
            result: Some(result),
            unreported_failure,
        };
    }
}

struct StopOnFailure {
    stop: CancellationToken,
    armed: bool,
}
impl StopOnFailure {
    fn disarm(mut self) {
        self.armed = false;
    }
}
impl Drop for StopOnFailure {
    fn drop(&mut self) {
        if self.armed {
            self.stop.cancel();
        }
    }
}

/// Read-only liveness of the actual task, independent of observer delivery or
/// whether its controlling owner has joined it. This grants no abort authority.
#[derive(Clone)]
pub struct OperationFence(Option<tokio::task::AbortHandle>);
impl OperationFence {
    pub fn is_finished(&self) -> bool {
        self.0
            .as_ref()
            .is_none_or(tokio::task::AbortHandle::is_finished)
    }
}

/// An owned stop-and-join handle, not authority to abort execution. Dropping it
/// requests stop; the Session retains the task and its outcome until joined.
/// Await a retained `&mut Operation` when selecting repeatedly on completion.
#[must_use = "retain and join the operation; dropping it only requests stop"]
pub struct Operation<T, E> {
    owner: Option<Arc<OperationOwner>>,
    id: u64,
    stop: CancellationToken,
    completion: Arc<Completion<T, E>>,
    joined: bool,
    fence: OperationFence,
    notified: Option<Pin<Box<OwnedNotified>>>,
}

impl<T, E> Unpin for Operation<T, E> {}

impl<T, E> Operation<T, E> {
    pub(crate) fn ready(result: Result<T, E>) -> Self {
        Self {
            owner: None,
            id: 0,
            stop: CancellationToken::new(),
            completion: Arc::new(Completion(Mutex::new(Outcome {
                result: Some(result),
                unreported_failure: None,
            }))),
            joined: false,
            fence: OperationFence(None),
            notified: None,
        }
    }

    pub fn request_stop(&self) {
        self.stop.cancel();
    }

    pub fn fence(&self) -> OperationFence {
        self.fence.clone()
    }
}

impl<T, E: From<OperationError>> Future for Operation<T, E> {
    type Output = Result<T, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(owner) = &this.owner {
            match owner.poll_join(
                this.id,
                cx,
                this.notified
                    .as_mut()
                    .expect("running operation notification"),
            ) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => {
                    this.joined = true;
                    return Poll::Ready(Err(error.into()));
                }
                Poll::Ready(Ok(())) => {}
            }
        }
        this.joined = true;
        let mut outcome = this
            .completion
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        outcome.unreported_failure = None;
        Poll::Ready(
            outcome
                .result
                .take()
                .unwrap_or_else(|| Err(OperationError::CompletionConsumed.into())),
        )
    }
}

impl<T, E> Drop for Operation<T, E> {
    fn drop(&mut self) {
        if !self.joined {
            self.stop.cancel();
        }
    }
}

pub(crate) struct OperationOwner {
    state: Mutex<State>,
    closed: AtomicBool,
    contended: AtomicBool,
    wake: Arc<Notify>,
    join_waker: Waker,
}

struct JoinWake(Arc<Notify>);
impl Wake for JoinWake {
    fn wake(self: Arc<Self>) {
        self.0.notify_waiters();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify_waiters();
    }
}

impl Default for OperationOwner {
    fn default() -> Self {
        let wake = Arc::new(Notify::new());
        Self {
            state: Mutex::new(State::default()),
            closed: AtomicBool::new(false),
            contended: AtomicBool::new(false),
            join_waker: Waker::from(Arc::new(JoinWake(wake.clone()))),
            wake,
        }
    }
}

#[derive(Default)]
struct State {
    next_id: u64,
    settled: u64,
    failure: Option<String>,
    running: Option<Running>,
    completed: Option<Arc<dyn CompletionCustody>>,
}

struct Running {
    id: u64,
    stop: CancellationToken,
    task: JoinHandle<()>,
    // Retain even an abandoned typed outcome. This is one in-process slot, not
    // another durable ledger; a subsequent operation replaces settled custody.
    _completion: Arc<dyn CompletionCustody>,
}

/// Synchronous preference/admission authority. The guard holds no async work.
/// A reversible idle reservation for synchronous preflight and publication.
/// It grants no execution authority. Do not hold it across asynchronous work.
pub struct IdleSession<'a>(StateGuard<'a>);

struct StateGuard<'a> {
    state: Option<MutexGuard<'a, State>>,
    owner: &'a OperationOwner,
    notify: bool,
    failed: Option<FailedTask>,
}

struct FailedTask {
    stop: CancellationToken,
    error: tokio::task::JoinError,
}
impl std::ops::Deref for StateGuard<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        self.state
            .as_deref()
            .expect("state guard is live until Drop")
    }
}
impl std::ops::DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut State {
        self.state
            .as_deref_mut()
            .expect("state guard is live until Drop")
    }
}
impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        drop(self.state.take());
        if let Some(failed) = &self.failed {
            failed.stop.cancel();
        }
        let contended = self.owner.contended.swap(false, Ordering::SeqCst);
        if self.notify || contended {
            self.owner.wake.notify_waiters();
        }
        // A JoinError can own an arbitrary panic payload with a destructor.
        if let Some(failed) = self.failed.take() {
            drop(failed.error);
        }
    }
}

impl OperationOwner {
    fn try_state(&self) -> Option<StateGuard<'_>> {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return None,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        };
        Some(StateGuard {
            state: Some(state),
            owner: self,
            notify: false,
            failed: None,
        })
    }

    fn poll_state<'a>(
        &'a self,
        cx: &mut Context<'_>,
        notified: &mut Pin<Box<OwnedNotified>>,
    ) -> Poll<StateGuard<'a>> {
        while notified.as_mut().poll(cx).is_ready() {
            *notified = Box::pin(self.wake.clone().notified_owned());
        }
        if let Some(state) = self.try_state() {
            return Poll::Ready(state);
        }
        self.contended.store(true, Ordering::SeqCst);
        // Recheck after registering contention: an unlock before registration
        // must not strand this waiter. Wake only on contention, not every poll.
        self.try_state().map_or(Poll::Pending, Poll::Ready)
    }

    pub(crate) fn try_lock(&self) -> Result<IdleSession<'_>, OperationError> {
        let mut state = self.try_state().ok_or(OperationError::Active)?;
        if state
            .running
            .as_ref()
            .is_some_and(|running| running.task.is_finished())
        {
            // Finished-task polling cannot suspend. Keep the handle in custody
            // until its actual completion (including failure) has been consumed.
            let result = (&mut state.running.as_mut().unwrap().task)
                .now_or_never()
                .expect("a finished task is ready to join");
            state.failed = state.finish(result);
            state.notify = true;
        }
        state.idle(self.closed.load(Ordering::SeqCst))?;
        Ok(IdleSession(state))
    }

    pub(crate) fn start<T, E, F>(
        self: &Arc<Self>,
        stop: CancellationToken,
        work: F,
    ) -> Operation<T, E>
    where
        T: Send + 'static,
        E: Send + From<OperationError> + OperationFailure + 'static,
        F: Future<Output = Result<T, E>> + Send + 'static,
    {
        match self.start_prepared(stop, || Ok(((), Some(work)))) {
            Ok(((), Some(operation))) => operation,
            Ok(((), None)) => unreachable!("ordinary operations always have work"),
            Err(error) => Operation::ready(Err(error)),
        }
    }

    /// Retain the actual blocking task, not an async wrapper that could finish
    /// while its blocking work still holds resources.
    pub(crate) fn start_blocking<T, E>(
        self: &Arc<Self>,
        stop: CancellationToken,
        work: impl FnOnce() -> Result<T, E> + Send + 'static,
    ) -> Operation<T, E>
    where
        T: Send + 'static,
        E: Send + From<OperationError> + OperationFailure + 'static,
    {
        let result = (|| {
            let runtime = tokio::runtime::Handle::try_current()
                .map_err(|_| E::from(OperationError::RuntimeUnavailable))?;
            let mut idle = self.try_lock().map_err(E::from)?;
            let (operation, previous) =
                self.install(&mut idle, stop, |completion, failure_stop| {
                    runtime.spawn_blocking(move || {
                        completion.finish(work());
                        failure_stop.disarm();
                    })
                });
            drop(idle);
            drop(previous);
            Ok(operation)
        })();
        result.unwrap_or_else(|error| Operation::ready(Err(error)))
    }

    /// Synchronous admission under exclusive custody, before publishing a receipt
    /// or allowing the task to run. Reconciliation may produce no new task.
    pub(crate) fn start_prepared<P, T, E, F>(
        self: &Arc<Self>,
        stop: CancellationToken,
        prepare: impl FnOnce() -> Result<(P, Option<F>), E>,
    ) -> Result<(P, Option<Operation<T, E>>), E>
    where
        T: Send + 'static,
        E: Send + From<OperationError> + OperationFailure + 'static,
        F: Future<Output = Result<T, E>> + Send + 'static,
    {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| E::from(OperationError::RuntimeUnavailable))?;
        let mut idle = self.try_lock().map_err(E::from)?;
        let (prepared, work) = prepare()?;
        let Some(work) = work else {
            return Ok((prepared, None));
        };
        let (operation, previous) = self.install(&mut idle, stop, |completion, failure_stop| {
            runtime.spawn(async move {
                completion.finish(work.await);
                failure_stop.disarm();
            })
        });
        drop(idle);
        drop(previous);
        Ok((prepared, Some(operation)))
    }

    fn install<T, E>(
        self: &Arc<Self>,
        idle: &mut IdleSession<'_>,
        stop: CancellationToken,
        spawn: impl FnOnce(Arc<Completion<T, E>>, StopOnFailure) -> JoinHandle<()>,
    ) -> (Operation<T, E>, Option<Arc<dyn CompletionCustody>>)
    where
        T: Send + 'static,
        E: Send + 'static,
    {
        // Reserve while still holding authority, before another runtime thread
        // can poll the task or a preference/replacement can observe it as idle.
        idle.0.next_id = idle
            .0
            .next_id
            .checked_add(1)
            .expect("operation identity exhausted");
        let previous = idle.0.completed.take();
        let id = idle.0.next_id;
        let completion = Arc::new(Completion(Mutex::new(Outcome {
            result: None,
            unreported_failure: None,
        })));
        let task = spawn(
            completion.clone(),
            StopOnFailure {
                stop: stop.clone(),
                armed: true,
            },
        );
        let fence = OperationFence(Some(task.abort_handle()));
        idle.0.running = Some(Running {
            id,
            stop: stop.clone(),
            task,
            _completion: completion.clone(),
        });
        (
            Operation {
                owner: Some(self.clone()),
                id,
                stop,
                completion,
                joined: false,
                fence,
                notified: Some(Box::pin(self.wake.clone().notified_owned())),
            },
            previous,
        )
    }

    fn poll_join(
        &self,
        id: u64,
        cx: &mut Context<'_>,
        notified: &mut Pin<Box<OwnedNotified>>,
    ) -> Poll<Result<(), OperationError>> {
        // Register each waiter before polling the retained join. A single task
        // waker fans out to operation and shutdown waiters; polling JoinHandle
        // with competing caller wakers would strand whichever was overwritten.
        let mut state = std::task::ready!(self.poll_state(cx, notified));
        if id <= state.settled {
            return Poll::Ready(Ok(()));
        }
        if let Some(failure) = &state.failure {
            return Poll::Ready(Err(OperationError::TaskFailed(failure.clone())));
        }
        let Some(running) = state.running.as_mut() else {
            return Poll::Ready(Err(OperationError::CompletionConsumed));
        };
        debug_assert_eq!(running.id, id);
        match Pin::new(&mut running.task).poll(&mut Context::from_waker(&self.join_waker)) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                state.failed = state.finish(result);
                state.notify = true;
                Poll::Ready(state.failure.as_ref().map_or(Ok(()), |failure| {
                    Err(OperationError::TaskFailed(failure.clone()))
                }))
            }
        }
    }

    /// Close admission before waiting. Cancellation of this wait leaves both
    /// closure and task custody intact. Runtime destruction is not shutdown.
    pub(crate) async fn shutdown(&self) -> Result<(), OperationError> {
        self.closed.store(true, Ordering::SeqCst);
        self.join().await
    }

    /// Stop and join current work without changing admission. Callers allowing
    /// reuse must fence new starts themselves until this join has completed.
    pub(crate) async fn join(&self) -> Result<(), OperationError> {
        let mut notified = Box::pin(self.wake.clone().notified_owned());
        let id = poll_fn(|cx| {
            let state = std::task::ready!(self.poll_state(cx, &mut notified));
            let result = if let Some(running) = &state.running {
                Ok(Some((running.id, running.stop.clone())))
            } else if let Some(failure) = &state.failure {
                Err(OperationError::TaskFailed(failure.clone()))
            } else {
                Ok(None)
            };
            drop(state);
            Poll::Ready(result.map(|running| {
                running.map(|(id, stop)| {
                    stop.cancel();
                    id
                })
            }))
        })
        .await?;
        if let Some(id) = id {
            poll_fn(|cx| self.poll_join(id, cx, &mut notified)).await?;
        }
        poll_fn(|cx| {
            let state = std::task::ready!(self.poll_state(cx, &mut notified));
            Poll::Ready(state.completion_failure().map_or(Ok(()), |error| {
                Err(OperationError::UnreportedFailure(error))
            }))
        })
        .await
    }
}

impl State {
    fn idle(&self, closed: bool) -> Result<(), OperationError> {
        if let Some(failure) = &self.failure {
            return Err(OperationError::TaskFailed(failure.clone()));
        }
        if let Some(error) = self.completion_failure() {
            return Err(OperationError::UnreportedFailure(error));
        }
        if closed {
            return Err(OperationError::Closed);
        }
        if self.running.is_some() {
            return Err(OperationError::Active);
        }
        Ok(())
    }

    fn completion_failure(&self) -> Option<String> {
        self.completed
            .as_ref()
            .and_then(|completion| completion.unreported_failure())
    }

    fn finish(&mut self, result: Result<(), tokio::task::JoinError>) -> Option<FailedTask> {
        let failed = match result {
            Ok(()) => {
                self.settled = self.running.as_ref().unwrap().id;
                None
            }
            // A genuine panic can destroy an executor's joining continuation.
            // Do not claim local quiescence or allow reuse of this live owner.
            Err(error) => {
                self.failure = Some(error.to_string());
                Some(FailedTask {
                    stop: self.running.as_ref().unwrap().stop.clone(),
                    error,
                })
            }
        };
        // Keep the outcome after join as well; replacement of the slot occurs
        // only on the next successful start. No running handle is abandoned.
        let running = self.running.take().unwrap();
        self.completed = Some(running._completion);
        failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DropProbe {
        owner: std::sync::Weak<OperationOwner>,
        outside_lock: Arc<AtomicBool>,
    }
    impl Drop for DropProbe {
        fn drop(&mut self) {
            let owner = self.owner.upgrade().unwrap();
            self.outside_lock
                .store(owner.state.try_lock().is_ok(), Ordering::SeqCst);
        }
    }

    struct WakeProbe {
        owner: std::sync::Weak<OperationOwner>,
        woke: AtomicBool,
        locked: AtomicBool,
    }
    impl Wake for WakeProbe {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.woke.store(true, Ordering::SeqCst);
            self.locked.fetch_or(
                self.owner.upgrade().unwrap().state.try_lock().is_err(),
                Ordering::SeqCst,
            );
        }
    }
    fn probe(owner: &Arc<OperationOwner>) -> Arc<WakeProbe> {
        Arc::new(WakeProbe {
            owner: Arc::downgrade(owner),
            woke: AtomicBool::new(false),
            locked: AtomicBool::new(false),
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completed_task_notifies_only_after_idle_guard_unlocks() {
        let owner = Arc::new(OperationOwner::default());
        let operation =
            owner.start_blocking(
                CancellationToken::new(),
                || Ok::<_, crate::SessionError>(()),
            );
        while !operation.fence().is_finished() {
            tokio::task::yield_now().await;
        }
        let probe = probe(&owner);
        let waker = Waker::from(probe.clone());
        let mut notified = Box::pin(owner.wake.clone().notified_owned());
        assert!(
            notified
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        let idle = owner.try_lock().unwrap();
        drop(idle);
        operation.await.unwrap();
        assert!(probe.woke.load(Ordering::SeqCst));
        assert!(
            !probe.locked.load(Ordering::SeqCst),
            "completion woke an observer under the owner lock"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn join_cancels_observers_outside_owner_lock() {
        let owner = Arc::new(OperationOwner::default());
        let stop = CancellationToken::new();
        let worker_stop = stop.clone();
        let operation = owner.start(stop.clone(), async move {
            worker_stop.cancelled().await;
            Ok::<_, crate::SessionError>(())
        });
        let probe = probe(&owner);
        let waker = Waker::from(probe.clone());
        let mut cancelled = Box::pin(stop.cancelled());
        assert!(
            cancelled
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        owner.join().await.unwrap();
        operation.await.unwrap();
        assert!(probe.woke.load(Ordering::SeqCst));
        assert!(
            !probe.locked.load(Ordering::SeqCst),
            "join requested stop under the owner lock"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn panic_payload_drops_outside_owner_lock() {
        let owner = Arc::new(OperationOwner::default());
        let outside_lock = Arc::new(AtomicBool::new(false));
        let probe = DropProbe {
            owner: Arc::downgrade(&owner),
            outside_lock: outside_lock.clone(),
        };
        let operation = owner.start_blocking(
            CancellationToken::new(),
            move || -> Result<(), crate::SessionError> { std::panic::panic_any(probe) },
        );
        assert!(matches!(
            operation.await,
            Err(crate::SessionError::Operation(OperationError::TaskFailed(
                _
            )))
        ));
        assert!(
            outside_lock.load(Ordering::SeqCst),
            "panic payload destructor ran under the owner lock"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replacing_unread_success_drops_arbitrary_results_outside_owner_lock() {
        let owner = Arc::new(OperationOwner::default());
        let outside_lock = Arc::new(AtomicBool::new(false));
        let probe = DropProbe {
            owner: Arc::downgrade(&owner),
            outside_lock: outside_lock.clone(),
        };
        let operation = owner.start_blocking(CancellationToken::new(), move || {
            Ok::<_, crate::SessionError>(probe)
        });
        let fence = operation.fence();
        drop(operation);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !fence.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        owner
            .start_blocking(
                CancellationToken::new(),
                || Ok::<_, crate::SessionError>(()),
            )
            .await
            .unwrap();
        assert!(outside_lock.load(Ordering::SeqCst));
        owner.shutdown().await.unwrap();
    }
}
