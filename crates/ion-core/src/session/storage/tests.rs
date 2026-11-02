use std::{
    sync::{
        Condvar,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use futures_util::FutureExt;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::*;

fn fixture() -> (std::path::PathBuf, Arc<Session>) {
    let root = std::env::temp_dir().join(format!("ion-storage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let session = Arc::new(Session::create(root.join("session.sqlite"), Some(&root)).unwrap());
    (root, session)
}

/// Release is bounded by an OS condition variable, not a Tokio timer: even a
/// regression blocking the current-thread executor must let the test finish.
#[derive(Default)]
struct Gate {
    released: Mutex<bool>,
    changed: Condvar,
    timed_out: AtomicBool,
}

impl Gate {
    fn wait(&self) {
        let (_released, timeout) = self
            .changed
            .wait_timeout_while(
                self.released.lock().unwrap(),
                Duration::from_secs(2),
                |released| !*released,
            )
            .unwrap();
        self.timed_out.store(timeout.timed_out(), Ordering::SeqCst);
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

struct StoreHold {
    gate: Arc<Gate>,
    thread: Option<thread::JoinHandle<()>>,
}

impl StoreHold {
    async fn new(session: &Arc<Session>) -> Self {
        let gate = Arc::new(Gate::default());
        let worker_gate = gate.clone();
        let session = session.clone();
        let (entered, ready) = oneshot::channel();
        let thread = thread::spawn(move || {
            let _store = session.store.lock().unwrap();
            entered.send(()).unwrap();
            worker_gate.wait();
        });
        ready.await.unwrap();
        Self {
            gate,
            thread: Some(thread),
        }
    }

    async fn heartbeat(&self) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !self.gate.timed_out.load(Ordering::SeqCst),
            "Store work blocked the async executor until the OS deadline"
        );
    }
}

impl Drop for StoreHold {
    fn drop(&mut self) {
        self.gate.release();
        self.thread.take().unwrap().join().unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_waits_retain_actual_work_and_unread_failures_until_shutdown() {
    for fail in [false, true] {
        let (root, session) = fixture();
        let lane = session.io().unwrap();
        let gate = Arc::new(Gate::default());
        let worker_gate = gate.clone();
        let (entered, ready) = oneshot::channel();
        let mut wait = Box::pin(lane.run(move |session| {
            entered.send(()).unwrap();
            worker_gate.wait();
            session.set_name(Some("committed after abandoning the wait"))?;
            if fail {
                Err(SessionError::Io(std::io::Error::other(
                    "unread storage failure",
                )))
            } else {
                Ok(())
            }
        }));
        assert!(wait.as_mut().now_or_never().is_none());
        ready.await.unwrap();
        drop(wait);
        drop(lane);
        let mut shutdown = Box::pin(session.shutdown());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), shutdown.as_mut())
                .await
                .is_err(),
            "shutdown did not join the actual blocking work"
        );
        drop(shutdown);
        assert!(matches!(
            session.io(),
            Err(SessionError::Operation(OperationError::Closed))
        ));
        assert!(session.artifact_sink().start("closed").is_err());
        gate.release();
        let result = session.shutdown().await;
        if fail {
            let error = result.unwrap_err();
            assert!(matches!(error, SessionError::Storage(_)));
            assert!(
                error.to_string().contains("unread storage failure"),
                "{error}"
            );
        } else {
            result.unwrap();
        }
        assert_eq!(
            session.view().unwrap().name.as_deref(),
            Some("committed after abandoning the wait")
        );
        assert!(!gate.timed_out.load(Ordering::SeqCst));
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn lanes_are_serial_independent_and_closed_by_shutdown() {
    let (root, session) = fixture();
    let lane = session.io().unwrap();
    let independent = session.io().unwrap();
    let gate = Arc::new(Gate::default());
    let worker_gate = gate.clone();
    let (entered, ready) = oneshot::channel();
    let mut first = Box::pin(lane.run(move |session| {
        entered.send(()).unwrap();
        worker_gate.wait();
        session.entry_count()
    }));
    assert!(first.as_mut().now_or_never().is_none());
    ready.await.unwrap();
    assert!(matches!(
        lane.run::<()>(|_| panic!("overlapping work dispatched"))
            .await,
        Err(SessionError::OperationActive)
    ));
    let count: Result<u64, crate::AgentError> = independent
        .run_with(|session| Ok(session.entry_count()?))
        .await;
    assert_eq!(count.unwrap(), 0);
    gate.release();
    assert_eq!(first.await.unwrap(), 0);
    assert_eq!(lane.run(Session::entry_count).await.unwrap(), 0);
    session.shutdown().await.unwrap();
    assert!(matches!(
        lane.run::<()>(|_| panic!("closed work dispatched")).await,
        Err(SessionError::Operation(OperationError::Closed))
    ));
    // Closing the async facade does not remove privileged blocking inspection.
    assert!(session.view().unwrap().entries.is_empty());
    drop((lane, independent, session));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_store_reads_leave_the_current_thread_executor_responsive() {
    let (root, session) = fixture();
    let lane = session.io().unwrap();
    let hold = StoreHold::new(&session).await;
    let mut work = Box::pin(lane.run(Session::entry_count));
    assert!(work.as_mut().now_or_never().is_none());
    hold.heartbeat().await;
    assert!(work.as_mut().now_or_never().is_none());
    drop(hold);
    assert_eq!(work.await.unwrap(), 0);
    session.shutdown().await.unwrap();
    drop((lane, session));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn shell_admission_and_settlement_are_off_executor_and_joined_after_drop() {
    let (root, session) = fixture();
    let admission_hold = StoreHold::new(&session).await;
    let (executed, mut execution) = oneshot::channel();
    let (finish, finished) = oneshot::channel();
    let operation = session.run_user_shell(
        "synthetic shell".into(),
        false,
        CancellationToken::new(),
        move |_, _, _| async move {
            executed.send(()).unwrap();
            finished.await.unwrap();
            crate::ToolOutput::success(serde_json::json!({"observed":true}))
        },
    );
    admission_hold.heartbeat().await;
    assert!(matches!(
        execution.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    drop(admission_hold);
    execution.await.unwrap();
    let settlement_hold = StoreHold::new(&session).await;
    finish.send(()).unwrap();
    settlement_hold.heartbeat().await;
    drop(operation);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), session.shutdown())
            .await
            .is_err()
    );
    drop(settlement_hold);
    session.shutdown().await.unwrap();
    let view = session.view().unwrap();
    assert_eq!(view.entries.len(), 2);
    assert!(
        matches!(&view.entries[1], super::super::SessionEntry::UserShellSettled {
        outcome: super::super::UserShellOutcome::Observed { output }, ..
    } if output.value["observed"] == true)
    );
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn stop_during_admission_never_dispatches_the_shell() {
    let (root, session) = fixture();
    let hold = StoreHold::new(&session).await;
    let stop = CancellationToken::new();
    let operation = session.run_user_shell(
        "must not dispatch".into(),
        false,
        stop.clone(),
        |_, _, _| async { panic!("stopped shell dispatched") },
    );
    hold.heartbeat().await;
    stop.cancel();
    drop(hold);
    assert!(matches!(
        operation.await,
        Err(SessionError::UserShellCancelled)
    ));
    session.shutdown().await.unwrap();
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}
