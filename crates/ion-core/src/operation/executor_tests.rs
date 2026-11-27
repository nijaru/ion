use super::{task::*, tests::DropProbe, *};
use crate::SessionError;
use futures_util::task::AtomicWaker;
use std::collections::VecDeque;

/// Deliberately driven one queued task at a time, without entering a Tokio
/// runtime. Storage work is synchronous SQLite on the test thread, not a model
/// of asynchronous hosted storage or a production scheduler.
#[derive(Default)]
pub(crate) struct ManualExecutor {
    queue: Mutex<VecDeque<(Work, Arc<Join>)>>,
    join_probe: Mutex<Option<DropProbe>>,
}

enum Work {
    Async(AsyncWork),
    Blocking(BlockingWork),
}

#[derive(Default)]
struct Join {
    result: Mutex<Option<TaskResult>>,
    finished: Arc<AtomicBool>,
    waker: AtomicWaker,
}

impl Join {
    fn finish(&self, result: TaskResult) {
        *self.result.lock().unwrap() = Some(result);
        self.finished.store(true, Ordering::Release);
        self.waker.wake();
    }
}

impl Spawner for ManualExecutor {
    fn enqueue(&self, work: AsyncWork) -> HostTask {
        self.schedule(Work::Async(work))
    }

    fn enqueue_blocking(&self, work: BlockingWork) -> HostTask {
        self.schedule(Work::Blocking(work))
    }
}

impl ManualExecutor {
    fn schedule(&self, work: Work) -> HostTask {
        let state = Arc::new(Join::default());
        self.queue.lock().unwrap().push_back((work, state.clone()));
        let read = state.clone();
        let probe = self.join_probe.lock().unwrap().take();
        HostTask {
            join: Box::pin(poll_fn(move |cx| {
                let _probe = &probe; // Runs when the owner drops the join handle.
                read.waker.register(cx.waker());
                if read.finished.load(Ordering::Acquire) {
                    Poll::Ready(read.result.lock().unwrap().take().unwrap())
                } else {
                    Poll::Pending
                }
            })),
            finished: state.finished.clone(),
        }
    }

    pub(crate) fn executor(self: &Arc<Self>) -> Executor {
        Executor(Some(self.clone()))
    }

    fn step(&self) -> bool {
        let next = self.queue.lock().unwrap().pop_front();
        let Some((work, join)) = next else {
            return false;
        };
        match work {
            Work::Async(mut future) => {
                if future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
                {
                    self.queue
                        .lock()
                        .unwrap()
                        .push_back((Work::Async(future), join));
                    return true;
                }
            }
            Work::Blocking(work) => work(),
        }
        join.finish(Ok(()));
        true
    }

    pub(crate) fn complete<F: Future>(&self, future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        for _ in 0..64 {
            if let Poll::Ready(result) = future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
            {
                return result;
            }
            assert!(self.step(), "the future has no runnable work");
        }
        panic!("the fixture did not make progress");
    }
}

#[test]
fn unavailable_executor_refuses_before_preparing_admission() {
    let owner = Arc::new(OperationOwner::default());
    let result = owner.start_prepared::<(), (), SessionError, std::future::Ready<_>>(
        CancellationToken::new(),
        || panic!("preparation ran without a bound executor"),
    );
    assert!(matches!(
        result,
        Err(SessionError::Operation(OperationError::RuntimeUnavailable))
    ));
    assert!(owner.try_lock().is_ok());
}

#[test]
fn dropping_the_observer_does_not_release_running_work() {
    let executor = Arc::new(ManualExecutor::default());
    let owner = Arc::new(OperationOwner::new(executor.executor()));
    let stop = CancellationToken::new();
    let worker_stop = stop.clone();
    let (release, held) = tokio::sync::oneshot::channel();
    let work = async move {
        worker_stop.cancelled().await;
        held.await.unwrap();
        Ok::<(), SessionError>(())
    };
    let (_, operation) = owner
        .start_prepared(stop.clone(), || Ok(((), Some(work))))
        .unwrap();
    let operation = operation.unwrap();
    let fence = operation.fence();
    assert!(!fence.is_finished());
    assert!(executor.step());
    assert!(matches!(owner.try_lock(), Err(OperationError::Active)));
    drop(operation);
    assert!(stop.is_cancelled());
    let mut shutdown = std::pin::pin!(owner.shutdown());
    let mut cx = Context::from_waker(Waker::noop());
    assert!(shutdown.as_mut().poll(&mut cx).is_pending());
    assert!(executor.step());
    assert!(!fence.is_finished());
    assert!(shutdown.as_mut().poll(&mut cx).is_pending());
    release.send(()).unwrap();
    executor.complete(shutdown).unwrap();
    assert!(fence.is_finished());
    assert!(matches!(owner.try_lock(), Err(OperationError::Closed)));
}

#[test]
fn pending_async_admission_survives_abandoned_receipt_wait_and_joins_on_shutdown() {
    let executor = Arc::new(ManualExecutor::default());
    let owner = Arc::new(OperationOwner::new(executor.executor()));
    let stop = CancellationToken::new();
    let running = Arc::new(AtomicBool::new(false));
    let running_after_ack = running.clone();
    let (acknowledge, held_commit) = oneshot::channel();
    let mut waiting = Box::pin(owner.start_admitting(stop.clone(), async move {
        let receipt = held_commit.await.unwrap();
        Ok((
            receipt,
            Some(async move {
                running_after_ack.store(true, Ordering::SeqCst);
                Ok::<_, SessionError>(())
            }),
        ))
    }));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(waiting.as_mut().poll(&mut cx).is_pending());
    assert!(executor.step(), "the actual task entered its commit wait");
    assert!(owner.try_state().unwrap().running.is_some());
    assert!(matches!(owner.try_lock(), Err(OperationError::Active)));
    assert!(!running.load(Ordering::SeqCst));
    drop(waiting);
    assert!(
        !stop.is_cancelled(),
        "losing the admission observer is not Stop"
    );

    let mut shutdown = std::pin::pin!(owner.shutdown());
    assert!(shutdown.as_mut().poll(&mut cx).is_pending());
    assert!(
        stop.is_cancelled(),
        "shutdown reached the operation before commit completed"
    );
    assert!(executor.step());
    assert!(!running.load(Ordering::SeqCst));
    assert!(shutdown.as_mut().poll(&mut cx).is_pending());
    acknowledge.send(42).unwrap();
    executor.complete(shutdown).unwrap();
    assert!(
        running.load(Ordering::SeqCst),
        "a positive commit still requires settlement"
    );
    assert!(matches!(owner.try_lock(), Err(OperationError::Closed)));
}

#[test]
fn lost_dispatch_cannot_turn_an_admitted_operation_into_success_or_idle() {
    let executor = Arc::new(ManualExecutor::default());
    let owner = Arc::new(OperationOwner::new(executor.executor()));
    let outside_lock = Arc::new(AtomicBool::new(false));
    *executor.join_probe.lock().unwrap() = Some(DropProbe {
        owner: Arc::downgrade(&owner),
        outside_lock: outside_lock.clone(),
    });
    let operation = owner
        .start_blocking(CancellationToken::new(), || -> Result<(), SessionError> {
            panic!("the lost work must never run")
        });
    let fence = operation.fence();
    let (work, join) = executor.queue.lock().unwrap().pop_front().unwrap();
    drop(work);
    join.finish(Err(TaskFailure {
        message: "lost dispatch".into(),
        payload: None,
    }));
    assert!(fence.is_finished());
    assert!(
        matches!(owner.try_lock(), Err(OperationError::TaskFailed(error)) if error == "lost dispatch")
    );
    assert!(
        outside_lock.load(Ordering::SeqCst),
        "join handle dropped under the owner lock"
    );
    assert!(
        matches!(executor.complete(operation), Err(SessionError::Operation(OperationError::TaskFailed(error))) if error == "lost dispatch")
    );
}
