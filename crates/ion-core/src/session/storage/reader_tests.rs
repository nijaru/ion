use std::{sync::Condvar, thread, time::Duration};

use futures_util::FutureExt;
use ion_ai::{Content, Message, ModelRef, Role};
use tokio::sync::oneshot;

use super::*;

fn fixture() -> (std::path::PathBuf, Arc<Session>) {
    let root = std::env::temp_dir().join(format!("ion-reader-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let session = Arc::new(Session::create(root.join("session.sqlite"), Some(&root)).unwrap());
    (root, session)
}

fn input(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(text.into())],
        provider_replay: None,
    }
}

fn admit(session: &Session) -> (RequestKey, Message) {
    let key = RequestKey::new("reader-test".into(), "accepted".into()).unwrap();
    let input = input("accepted input");
    session
        .admit_turn(
            input.clone(),
            ModelRef {
                provider: "synthetic".into(),
                model: "model".into(),
            },
            Some(key.clone()),
            None,
        )
        .unwrap();
    (key, input)
}

#[tokio::test(flavor = "current_thread")]
async fn view_projection_runs_off_executor_outside_store_and_survives_an_abandoned_wait() {
    let (root, session) = fixture();
    let reader = Arc::new(session.reader().unwrap());
    session.shutdown().await.unwrap();
    let caller = thread::current().id();
    let inspected = session.clone();
    let (entered, ready) = oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let waiting = tokio::spawn(async move {
        reader
            .map_view(move |view| {
                entered
                    .send((
                        thread::current().id() != caller,
                        inspected.store().try_lock().is_ok(),
                        view.id,
                    ))
                    .unwrap();
                assert!(
                    released.recv_timeout(Duration::from_secs(2)).is_ok(),
                    "projection must not block its caller's executor"
                );
                view.id
            })
            .await
    });
    let (off_executor, unlocked, id) = ready.await.unwrap();
    assert!(off_executor);
    assert!(
        unlocked,
        "application projection must not hold Store custody"
    );
    assert_eq!(id, session.id());
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    let mut shutdown = Box::pin(session.shutdown());
    assert!(futures_util::poll!(&mut shutdown).is_pending());
    tokio::time::sleep(Duration::from_millis(20)).await;
    release.send(()).unwrap();
    shutdown.await.unwrap();
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

/// A bounded real Store lock wait lets public reader methods exercise blocking
/// work without adding a callback or test hook to the reader capability.
struct StoreHold {
    release: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<thread::JoinHandle<bool>>,
}

impl StoreHold {
    async fn new(session: &Arc<Session>) -> Self {
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let signal = release.clone();
        let session = session.clone();
        let (entered, ready) = oneshot::channel();
        let worker = thread::spawn(move || {
            let _store = session.store().lock().unwrap();
            entered.send(()).unwrap();
            let (_released, timeout) = signal
                .1
                .wait_timeout_while(
                    signal.0.lock().unwrap(),
                    Duration::from_secs(2),
                    |released| !*released,
                )
                .unwrap();
            timeout.timed_out()
        });
        ready.await.unwrap();
        Self {
            release,
            worker: Some(worker),
        }
    }
}

impl Drop for StoreHold {
    fn drop(&mut self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
        assert!(
            !self.worker.take().unwrap().join().unwrap(),
            "read blocked the current-thread executor until the OS deadline"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn completed_shutdown_allows_typed_reads_but_never_reopens_write_lanes() {
    let (root, session) = fixture();
    let (key, accepted) = admit(&session);
    session.set_name(Some("final snapshot")).unwrap();
    let io = session.io().unwrap();
    let reader = session.reader().unwrap();
    session.shutdown().await.unwrap();

    assert_eq!(reader.view().await.unwrap().unfinished_turn, Some(1));
    let status = reader.status().await.unwrap();
    assert_eq!(status.name.as_deref(), Some("final snapshot"));
    assert_eq!(status.entries, 1);
    let receipt = reader.request_receipt(&key).await.unwrap().unwrap();
    assert_eq!(receipt.session, session.id());
    assert_eq!(receipt.turn, 1);
    assert_eq!(
        reader.reconcile_request(&key, &accepted).await.unwrap(),
        Some(receipt)
    );
    assert!(matches!(
        reader
            .reconcile_request(&key, &input("different input"))
            .await,
        Err(SessionError::RequestConflict)
    ));
    let missing = RequestKey::new("reader-test".into(), "not accepted".into()).unwrap();
    assert!(reader.request_receipt(&missing).await.unwrap().is_none());
    assert!(matches!(
        io.run::<()>(|_| panic!("write-capable callback admitted after shutdown"))
            .await,
        Err(SessionError::Operation(OperationError::Closed))
    ));
    assert!(matches!(
        session.io(),
        Err(SessionError::Operation(OperationError::Closed))
    ));
    assert!(session.artifact_sink().start("closed").is_err());

    let fresh_reader = session.reader().unwrap();
    assert_eq!(fresh_reader.status().await.unwrap().entries, 1);
    session.shutdown().await.unwrap();
    // Shutdown joins reader lanes without permanently closing their owners.
    assert_eq!(reader.status().await.unwrap().entries, 1);
    drop((fresh_reader, reader, io, session));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_post_shutdown_read_retains_lease_and_is_joined_before_reopening_reads() {
    let (root, session) = fixture();
    session.shutdown().await.unwrap();
    let reader = session.reader().unwrap();
    let hold = StoreHold::new(&session).await;
    let mut read = Box::pin(reader.view());
    assert!(read.as_mut().now_or_never().is_none());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(read.as_mut().now_or_never().is_none());
    assert!(matches!(
        reader.status().await,
        Err(SessionError::OperationActive)
    ));
    drop(read);

    assert!(
        tokio::time::timeout(Duration::from_millis(20), session.shutdown())
            .await
            .is_err(),
        "shutdown returned before the abandoned actual read finished"
    );
    assert!(matches!(
        session.reader(),
        Err(SessionError::Operation(OperationError::Closed))
    ));
    assert!(matches!(
        reader.view().await,
        Err(SessionError::Operation(OperationError::Closed))
    ));
    assert!(matches!(
        session.io(),
        Err(SessionError::Operation(OperationError::Closed))
    ));
    assert!(matches!(
        Session::open(session.path()),
        Err(SessionError::AlreadyOpen)
    ));

    // Cancellation leaves Closing intact until a retry actually joins work.
    tokio::task::yield_now().await;
    assert!(matches!(
        session.reader(),
        Err(SessionError::Operation(OperationError::Closed))
    ));
    drop(hold);
    session.shutdown().await.unwrap();
    assert!(reader.view().await.unwrap().entries.is_empty());
    let path = session.path().to_owned();
    drop(session);
    assert!(matches!(
        Session::open(&path),
        Err(SessionError::AlreadyOpen)
    ));
    drop(reader);
    let reopened = Session::open(&path).unwrap();
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_waits_for_admission_in_flight_and_excludes_later_work() {
    let (root, session) = fixture();
    let io = session.io().unwrap();
    let reader = session.reader().unwrap();
    let admission = session.storage.admit(Access::ReadWrite).unwrap();
    let mut shutdown = Box::pin(session.shutdown());
    assert!(shutdown.as_mut().now_or_never().is_none());
    assert!(matches!(
        io.run::<()>(|_| panic!("write raced closing admission"))
            .await,
        Err(SessionError::Operation(OperationError::Closed))
    ));
    assert!(matches!(
        reader.view().await,
        Err(SessionError::Operation(OperationError::Closed))
    ));
    drop(admission);
    shutdown.await.unwrap();
    assert!(reader.view().await.unwrap().entries.is_empty());
    assert!(matches!(
        io.run::<()>(|_| panic!("write raced closed admission"))
            .await,
        Err(SessionError::Operation(OperationError::Closed))
    ));
    drop((io, reader, session));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_read_failure_survives_join_and_diagnostics_remain_available() {
    let (root, session) = fixture();
    let (key, _) = admit(&session);
    session.shutdown().await.unwrap();
    let reader = session.reader().unwrap();
    let hold = StoreHold::new(&session).await;
    let conflicting = input("conflicting input");
    let mut read = Box::pin(reader.reconcile_request(&key, &conflicting));
    assert!(read.as_mut().now_or_never().is_none());
    drop(read);
    drop(hold);

    let error = session.shutdown().await.unwrap_err();
    assert!(matches!(error, SessionError::Storage(_)));
    assert!(
        error
            .to_string()
            .contains("already accepted with different input")
    );
    assert!(matches!(
        reader.status().await,
        Err(SessionError::Operation(OperationError::UnreportedFailure(
            _
        )))
    ));
    let diagnostic = session.reader().unwrap();
    assert_eq!(diagnostic.view().await.unwrap().unfinished_turn, Some(1));
    assert!(matches!(
        session.io(),
        Err(SessionError::Operation(OperationError::Closed))
    ));
    // Failure reporting cannot short-circuit joining another admitted read.
    let hold = StoreHold::new(&session).await;
    let mut read = Box::pin(diagnostic.status());
    assert!(read.as_mut().now_or_never().is_none());
    drop(read);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), session.shutdown())
            .await
            .is_err()
    );
    drop(hold);
    assert!(session.shutdown().await.is_err());
    assert_eq!(diagnostic.status().await.unwrap().entries, 1);
    drop((reader, diagnostic, session));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn actual_read_retains_writer_lease_after_all_public_handles_are_dropped() {
    let (root, session) = fixture();
    let path = session.path().to_owned();
    session.shutdown().await.unwrap();
    let reader = session.reader().unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let mut read = Box::pin(reader.run(move |session| {
        entered.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(2)).unwrap();
        session.view()
    }));
    assert!(read.as_mut().now_or_never().is_none());
    ready.await.unwrap();
    drop(read);
    drop((reader, session));
    assert!(matches!(
        Session::open(&path),
        Err(SessionError::AlreadyOpen)
    ));
    release.send(()).unwrap();
    let reopened = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match Session::open(&path) {
                Ok(session) => break session,
                Err(SessionError::AlreadyOpen) => tokio::task::yield_now().await,
                Err(error) => panic!("reopen failed: {error}"),
            }
        }
    })
    .await
    .unwrap();
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn pruning_unread_success_and_rejecting_captures_drop_outside_registry_lock() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Probe {
        session: std::sync::Weak<Session>,
        dropped: Arc<AtomicBool>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            let session = self.session.upgrade().unwrap();
            assert!(
                session.storage.state.try_lock().is_ok(),
                "user Drop under registry lock"
            );
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    let (root, session) = fixture();
    let io = session.io().unwrap();
    let rejected_io = session.io().unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let probe = Probe {
        session: Arc::downgrade(&session),
        dropped: dropped.clone(),
    };
    let (entered, ready) = oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let mut read = Box::pin(io.run(move |_| {
        entered.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(2)).unwrap();
        Ok(probe)
    }));
    assert!(read.as_mut().now_or_never().is_none());
    ready.await.unwrap();
    drop(read);
    drop(io);
    release.send(()).unwrap();
    session.shutdown().await.unwrap();
    assert!(
        !dropped.load(Ordering::SeqCst),
        "unread outcome was not retained"
    );
    let reader = session.reader().unwrap();
    assert!(
        dropped.load(Ordering::SeqCst),
        "settled owner was not pruned"
    );

    let rejected = Arc::new(AtomicBool::new(false));
    let probe = Probe {
        session: Arc::downgrade(&session),
        dropped: rejected.clone(),
    };
    assert!(
        rejected_io
            .run(move |_| {
                drop(probe);
                Ok(())
            })
            .await
            .is_err()
    );
    assert!(rejected.load(Ordering::SeqCst));
    drop((rejected_io, reader, session));
    std::fs::remove_dir_all(root).unwrap();
}
