//! Launch and join mechanics, not authority to admit or stop an operation.
use super::OperationError;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
};
use tokio::{
    runtime::Handle,
    task::{AbortHandle, JoinError, JoinHandle, unconstrained},
};

pub(super) struct TaskFailure {
    pub(super) message: String,
    pub(super) payload: Option<Box<dyn std::any::Any + Send>>,
}

impl From<JoinError> for TaskFailure {
    fn from(error: JoinError) -> Self {
        Self {
            message: error.to_string(),
            payload: error.try_into_panic().ok(),
        }
    }
}

pub(super) type TaskResult = Result<(), TaskFailure>;

pub(super) type AsyncWork = Pin<Box<dyn Future<Output = ()> + Send>>;
pub(super) type BlockingWork = Box<dyn FnOnce() + Send>;

/// An already-bound scheduler. Enqueue must not run or drop the work inline:
/// the owner still holds admission authority. Dispatch returns a handle for
/// the actual work, including if rejected. A rejected/lost task must join as
/// a failure, not a successful no-op.
pub(super) trait Spawner: Send + Sync {
    fn enqueue(&self, work: AsyncWork) -> HostTask;
    fn enqueue_blocking(&self, work: BlockingWork) -> HostTask;
}

pub(super) struct HostTask {
    /// A join-only future. Polling never drives the work. Once `finished` is
    /// true, the work (including its destructors) has ended and its outcome
    /// can be taken without yielding. The latch stays true after the join.
    pub(super) join: Pin<Box<dyn Future<Output = TaskResult> + Send>>,
    pub(super) finished: Arc<AtomicBool>,
}

#[derive(Clone, Default)]
pub(crate) struct Executor(pub(super) Option<Arc<dyn Spawner>>);

impl Executor {
    pub(super) fn bind(&self) -> Result<BoundSpawner<'_>, OperationError> {
        match self.0.as_deref() {
            Some(host) => Ok(BoundSpawner::Host(host)),
            None => Handle::try_current()
                .map(BoundSpawner::Tokio)
                .map_err(|_| OperationError::RuntimeUnavailable),
        }
    }
}

pub(super) enum BoundSpawner<'a> {
    Tokio(Handle),
    Host(&'a dyn Spawner),
}

impl BoundSpawner<'_> {
    pub(super) fn spawn(self, work: impl Future<Output = ()> + Send + 'static) -> Task {
        match self {
            Self::Tokio(runtime) => Task::Tokio(runtime.spawn(work)),
            Self::Host(host) => Task::Host(host.enqueue(Box::pin(work))),
        }
    }

    pub(super) fn spawn_blocking(self, work: impl FnOnce() + Send + 'static) -> Task {
        match self {
            Self::Tokio(runtime) => Task::Tokio(runtime.spawn_blocking(work)),
            Self::Host(host) => Task::Host(host.enqueue_blocking(Box::new(work))),
        }
    }
}

pub(super) enum Task {
    Tokio(JoinHandle<()>),
    Host(HostTask),
}

#[derive(Clone)]
enum Liveness {
    Tokio(AbortHandle),
    Host(Arc<AtomicBool>),
}

/// Read-only liveness of the actual task, independent of observer delivery or
/// whether its controlling owner has joined it. This grants no abort authority.
#[derive(Clone)]
pub struct OperationFence(Option<Liveness>);

impl OperationFence {
    pub(super) fn completed() -> Self {
        Self(None)
    }

    pub fn is_finished(&self) -> bool {
        match &self.0 {
            None => true,
            Some(Liveness::Tokio(task)) => task.is_finished(),
            Some(Liveness::Host(finished)) => finished.load(Ordering::Acquire),
        }
    }
}

impl Task {
    pub(super) fn fence(&self) -> OperationFence {
        OperationFence(Some(match self {
            Self::Tokio(task) => Liveness::Tokio(task.abort_handle()),
            Self::Host(task) => Liveness::Host(task.finished.clone()),
        }))
    }

    pub(super) fn poll_join(&mut self, cx: &mut Context<'_>) -> Poll<TaskResult> {
        match self {
            Self::Tokio(task) => Pin::new(task).poll(cx).map_err(Into::into),
            Self::Host(task) => task.join.as_mut().poll(cx),
        }
    }

    pub(super) fn try_join(&mut self) -> Option<TaskResult> {
        let cx = &mut Context::from_waker(Waker::noop());
        let result = match self {
            Self::Tokio(task) if task.is_finished() => {
                // Joining an already-finished task is part of synchronous idle
                // admission. Tokio's cooperative budget can otherwise force a
                // yield even though the task's output is already available.
                Pin::new(&mut unconstrained(task))
                    .poll(cx)
                    .map_err(Into::into)
            }
            Self::Host(task) if task.finished.load(Ordering::Acquire) => {
                task.join.as_mut().poll(cx)
            }
            _ => return None,
        };
        match result {
            Poll::Ready(result) => Some(result),
            Poll::Pending => unreachable!("a finished task must be ready to join"),
        }
    }
}
