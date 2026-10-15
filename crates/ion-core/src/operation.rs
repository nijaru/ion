//! Exclusive, retained custody of one Session operation and its actual task.
use futures_util::FutureExt;
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
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

struct StopOnFailure {
    stop: CancellationToken,
    armed: bool,
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
#[must_use = "retain and join the operation; dropping it only requests stop"]
pub struct Operation<T, E> {
    owner: Option<Arc<OperationOwner>>,
    id: u64,
    stop: CancellationToken,
    completion: Arc<Completion<T, E>>,
    joined: bool,
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
            notified: None,
        }
    }

    pub fn request_stop(&self) {
        self.stop.cancel();
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
            join_waker: Waker::from(Arc::new(JoinWake(wake.clone()))),
            wake,
        }
    }
}

#[derive(Default)]
struct State {
    closed: bool,
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
pub struct IdleSession<'a>(MutexGuard<'a, State>);

impl OperationOwner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn try_lock(&self) -> Result<IdleSession<'_>, OperationError> {
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => return Err(OperationError::Active),
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        };
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
            state.finish(result);
            self.wake.notify_waiters();
        }
        state.idle()?;
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
        // Reserve while still holding authority, before another runtime thread
        // can poll the task or a preference/replacement can observe it as idle.
        idle.0.next_id = idle
            .0
            .next_id
            .checked_add(1)
            .expect("operation identity exhausted");
        idle.0.completed = None;
        let id = idle.0.next_id;
        let completion = Arc::new(Completion(Mutex::new(Outcome {
            result: None,
            unreported_failure: None,
        })));
        let result = completion.clone();
        let failure_stop = StopOnFailure {
            stop: stop.clone(),
            armed: true,
        };
        let task = runtime.spawn(async move {
            let mut failure_stop = failure_stop;
            let outcome = work.await;
            let unreported_failure = outcome
                .as_ref()
                .err()
                .filter(|error| !error.expected_cancellation())
                .map(ToString::to_string);
            *result
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Outcome {
                result: Some(outcome),
                unreported_failure,
            };
            failure_stop.armed = false;
        });
        idle.0.running = Some(Running {
            id,
            stop: stop.clone(),
            task,
            _completion: completion.clone(),
        });
        Ok((
            prepared,
            Some(Operation {
                owner: Some(self.clone()),
                id,
                stop,
                completion,
                joined: false,
                notified: Some(Box::pin(self.wake.clone().notified_owned())),
            }),
        ))
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
        while notified.as_mut().poll(cx).is_ready() {
            *notified = Box::pin(self.wake.clone().notified_owned());
        }
        let mut state = self.lock();
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
                state.finish(result);
                self.wake.notify_waiters();
                Poll::Ready(state.failure.as_ref().map_or(Ok(()), |failure| {
                    Err(OperationError::TaskFailed(failure.clone()))
                }))
            }
        }
    }

    /// Close admission before waiting. Cancellation of this wait leaves both
    /// closure and task custody intact. Runtime destruction is not shutdown.
    pub(crate) async fn shutdown(&self) -> Result<(), OperationError> {
        let id = {
            let mut state = self.lock();
            state.closed = true;
            if let Some(running) = &state.running {
                running.stop.cancel();
                Some(running.id)
            } else if let Some(failure) = &state.failure {
                return Err(OperationError::TaskFailed(failure.clone()));
            } else {
                None
            }
        };
        if let Some(id) = id {
            let mut notified = Box::pin(self.wake.clone().notified_owned());
            poll_fn(|cx| self.poll_join(id, cx, &mut notified)).await?;
        }
        self.lock().completion_failure().map_or(Ok(()), |error| {
            Err(OperationError::UnreportedFailure(error))
        })
    }
}

impl State {
    fn idle(&self) -> Result<(), OperationError> {
        if let Some(failure) = &self.failure {
            return Err(OperationError::TaskFailed(failure.clone()));
        }
        if let Some(error) = self.completion_failure() {
            return Err(OperationError::UnreportedFailure(error));
        }
        if self.closed {
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

    fn finish(&mut self, result: Result<(), tokio::task::JoinError>) {
        match result {
            Ok(()) => self.settled = self.running.as_ref().unwrap().id,
            // A genuine panic can destroy an executor's joining continuation.
            // Do not claim local quiescence or allow reuse of this live owner.
            Err(error) => {
                self.running.as_ref().unwrap().stop.cancel();
                self.failure = Some(error.to_string());
            }
        }
        // Keep the outcome after join as well; replacement of the slot occurs
        // only on the next successful start. No running handle is abandoned.
        let running = self.running.take().unwrap();
        self.completed = Some(running._completion);
    }
}
