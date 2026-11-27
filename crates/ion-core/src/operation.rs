//! Exclusive, retained custody of one Session operation and its actual task.
mod task;

use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};
pub(crate) use task::Executor;
pub use task::OperationFence;
use task::{Task, TaskResult};
use thiserror::Error;
use tokio::sync::{Notify, futures::OwnedNotified, oneshot};
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

/// An owned stop-and-join handle, not authority to abort execution. Dropping it
/// requests stop; the Session retains the task and its outcome until joined.
/// Await a retained `&mut Operation` when selecting repeatedly on completion.
///
/// Core execution entry points use caller tokens as parent cancellation scopes.
/// Stopping this operation cancels its work and descendants, not that parent or
/// sibling operations. Cancelling the parent still stops this operation.
#[must_use = "retain and join the operation; dropping it only requests stop"]
pub struct Operation<T, E> {
    owner: Option<Arc<OperationOwner>>,
    id: u64,
    stop: CancellationToken,
    completion: Arc<Completion<T, E>>,
    joined: bool,
    stop_on_drop: bool,
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
            stop_on_drop: true,
            fence: OperationFence::completed(),
            notified: None,
        }
    }

    pub fn request_stop(&self) {
        self.stop.cancel();
    }

    pub fn fence(&self) -> OperationFence {
        self.fence.clone()
    }

    fn poll_task(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), OperationError>> {
        let result = match &self.owner {
            Some(owner) => std::task::ready!(
                owner.poll_join(
                    self.id,
                    cx,
                    self.notified
                        .as_mut()
                        .expect("running operation notification"),
                )
            ),
            None => Ok(()),
        };
        self.joined = true;
        Poll::Ready(result)
    }
}

impl<T, E: From<OperationError>> Future for Operation<T, E> {
    type Output = Result<T, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        std::task::ready!(this.poll_task(cx))?;
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
        if !self.joined && self.stop_on_drop {
            self.stop.cancel();
        }
    }
}

pub(crate) struct OperationOwner {
    executor: Executor,
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
        Self::new(Executor::default())
    }
}

impl OperationOwner {
    pub(crate) fn new(executor: Executor) -> Self {
        let wake = Arc::new(Notify::new());
        Self {
            executor,
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
    task: Task,
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
    joined: Option<JoinedTask>,
}

struct JoinedTask {
    // A host's retained join handle can own arbitrary destructors, too.
    _task: Task,
    failure: Option<FailedTask>,
}

struct FailedTask {
    stop: CancellationToken,
    _payload: Option<Box<dyn std::any::Any + Send>>,
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
        if let Some(failed) = self
            .joined
            .as_ref()
            .and_then(|joined| joined.failure.as_ref())
        {
            failed.stop.cancel();
        }
        let contended = self.owner.contended.swap(false, Ordering::SeqCst);
        if self.notify || contended {
            self.owner.wake.notify_waiters();
        }
        // Drop the join handle and any panic payload only after unlocking.
        drop(self.joined.take());
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
            joined: None,
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
        if let Some(result) = state.running.as_mut().and_then(|run| run.task.try_join()) {
            state.joined = Some(state.finish(result));
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
            let spawner = self.executor.bind().map_err(E::from)?;
            let mut idle = self.try_lock().map_err(E::from)?;
            let (operation, previous) =
                self.install(&mut idle, stop, |completion, failure_stop| {
                    spawner.spawn_blocking(move || {
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
        let spawner = self.executor.bind().map_err(E::from)?;
        let mut idle = self.try_lock().map_err(E::from)?;
        let (prepared, work) = prepare()?;
        let Some(work) = work else {
            return Ok((prepared, None));
        };
        let (operation, previous) = self.install(&mut idle, stop, |completion, failure_stop| {
            spawner.spawn(async move {
                completion.finish(work.await);
                failure_stop.disarm();
            })
        });
        drop(idle);
        drop(previous);
        Ok((prepared, Some(operation)))
    }

    /// The retained task owns admission and the work it authorizes. The
    /// caller observes the decision without acquiring stop-on-drop custody
    /// until it receives a new admission. Shutdown can stop and join at either
    /// phase; the prepare future must drive any started commit to its outcome.
    pub(crate) async fn start_admitting<P, T, E, A, F>(
        self: &Arc<Self>,
        stop: CancellationToken,
        prepare: A,
    ) -> Result<(P, Option<Operation<T, E>>), E>
    where
        P: Send + 'static,
        T: Send + 'static,
        E: Send + From<OperationError> + OperationFailure + 'static,
        A: Future<Output = Result<(P, Option<F>), E>> + Send + 'static,
        F: Future<Output = Result<T, E>> + Send + 'static,
    {
        let (acknowledge, decision) = oneshot::channel();
        let mut operation = {
            let spawner = self.executor.bind().map_err(E::from)?;
            let mut idle = self.try_lock().map_err(E::from)?;
            let (operation, previous) =
                self.install(&mut idle, stop, |completion, failure_stop| {
                    spawner.spawn(async move {
                        match prepare.await {
                            Ok((prepared, work)) => {
                                let _ = acknowledge.send((prepared, work.is_some()));
                                if let Some(work) = work {
                                    completion.finish(work.await);
                                }
                            }
                            Err(error) => completion.finish(Err(error)),
                        }
                        failure_stop.disarm();
                    })
                });
            drop(idle);
            drop(previous);
            operation
        };
        operation.stop_on_drop = false;
        match decision.await {
            Ok((prepared, true)) => {
                operation.stop_on_drop = true;
                Ok((prepared, Some(operation)))
            }
            Ok((prepared, false)) => {
                // Passive reconciliation has no execution result, but must
                // still join its actual admission task before returning.
                poll_fn(|cx| operation.poll_task(cx)).await?;
                Ok((prepared, None))
            }
            // Failure or scheduler loss: join the task and surface the real
            // error, including uncertainty. No admission was acknowledged.
            Err(_) => {
                operation.await?;
                Err(OperationError::CompletionConsumed.into())
            }
        }
    }

    fn install<T, E>(
        self: &Arc<Self>,
        idle: &mut IdleSession<'_>,
        stop: CancellationToken,
        spawn: impl FnOnce(Arc<Completion<T, E>>, StopOnFailure) -> Task,
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
        let fence = task.fence();
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
                stop_on_drop: true,
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
        // waker fans out to operation and shutdown waiters; polling the join
        // with competing caller wakers could strand whichever was overwritten.
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
        match running
            .task
            .poll_join(&mut Context::from_waker(&self.join_waker))
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                state.joined = Some(state.finish(result));
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

    fn finish(&mut self, result: TaskResult) -> JoinedTask {
        let running = self.running.take().expect("joining the retained task");
        let failure = match result {
            Ok(()) => {
                self.settled = running.id;
                None
            }
            // A genuine panic can destroy an executor's joining continuation.
            // Do not claim local quiescence or allow reuse of this live owner.
            Err(error) => {
                self.failure = Some(error.message);
                Some(FailedTask {
                    stop: running.stop,
                    _payload: error.payload,
                })
            }
        };
        // Keep the outcome after join as well; replacement of the slot occurs
        // only on the next successful start. No running handle is abandoned.
        self.completed = Some(running._completion);
        JoinedTask {
            _task: running.task,
            failure,
        }
    }
}

#[cfg(test)]
pub(crate) mod executor_tests;

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) struct DropProbe {
        pub(super) owner: std::sync::Weak<OperationOwner>,
        pub(super) outside_lock: Arc<AtomicBool>,
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
        tokio::spawn({
            let owner = owner.clone();
            async move {
                // A synchronous reservation must join an already-finished
                // task even when its caller has exhausted Tokio's budget.
                use tokio::task::coop::{consume_budget, has_budget_remaining};
                while has_budget_remaining() {
                    consume_budget().await;
                }
                drop(owner.try_lock().unwrap());
            }
        })
        .await
        .unwrap();
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
