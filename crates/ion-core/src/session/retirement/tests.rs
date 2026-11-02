use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

use super::*;

#[test]
fn reader_last_release_keeps_actual_native_teardown_joinable() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = std::env::temp_dir().join(format!("ion-retire-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("session.db");
        let session = Arc::new(Session::create(&path, None).unwrap());
        let retirement = session.retirement();
        let reader = session.reader().unwrap();
        session.shutdown().await.unwrap();
        // Closing execution does not remove the authoritative Store or readers.
        assert_eq!(reader.status().await.unwrap().entries, 0);
        let (release, gate) = mpsc::channel();
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        waiting.await.unwrap();
        drop(session);
        drop(reader); // Last reference, on the sole async executor thread.
        assert!(retirement.try_join().is_none());
        assert!(matches!(
            Session::open(&path),
            Err(SessionError::AlreadyOpen)
        ));
        // A timer progresses while actual native close is queued. Cancelling
        // this borrowed join neither closes the lease nor detaches the task.
        assert!(
            tokio::time::timeout(Duration::from_millis(10), retirement.join())
                .await
                .is_err()
        );
        assert!(matches!(
            Session::open(&path),
            Err(SessionError::AlreadyOpen)
        ));
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), retirement.join())
            .await
            .unwrap()
            .unwrap();
        blocker.await.unwrap();
        assert!(retirement.try_join().unwrap().is_ok());
        let reopened = Session::open(path).unwrap();
        assert_eq!(reopened.status().unwrap().entries, 0);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    });
}

#[tokio::test(flavor = "current_thread")]
async fn unread_child_outcomes_retire_on_native_leaf_without_pinning_children() {
    use futures_util::FutureExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Witness {
        executor: std::thread::ThreadId,
        native: Arc<AtomicBool>,
    }
    impl Drop for Witness {
        fn drop(&mut self) {
            self.native.store(
                std::thread::current().id() != self.executor,
                Ordering::SeqCst,
            );
        }
    }
    let root = std::env::temp_dir().join(format!("ion-retire-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let parent_path = root.join("parent.db");
    let child_path = root.join("child.db");
    let parent = Arc::new(Session::create(&parent_path, None).unwrap());
    let retirement = parent.retirement();
    let io = parent.io().unwrap();
    let native = Arc::new(AtomicBool::new(false));
    let witness = Witness {
        executor: std::thread::current().id(),
        native: native.clone(),
    };
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, gate) = mpsc::channel();
    let path = child_path.clone();
    let mut read = Box::pin(io.run(move |parent| {
        let child = Arc::new(Session::create(path, None)?);
        parent.retain_child_resources(&child)?;
        entered.send(()).unwrap();
        gate.recv_timeout(Duration::from_secs(3)).unwrap();
        Ok((child, witness))
    }));
    assert!(read.as_mut().now_or_never().is_none());
    waiting.await.unwrap();
    drop(read); // Actual work and its unread child result stay retained.
    release.send(()).unwrap();
    parent.shutdown().await.unwrap();
    assert!(matches!(
        Session::open(&child_path),
        Err(SessionError::AlreadyOpen)
    ));
    drop(io);
    drop(parent);
    tokio::time::timeout(Duration::from_secs(3), retirement.join())
        .await
        .unwrap()
        .unwrap();
    assert!(
        native.load(Ordering::SeqCst),
        "arbitrary unread result destructor ran on the executor"
    );
    drop(Session::open(parent_path).unwrap());
    drop(Session::open(child_path).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn retirement_does_not_revoke_an_external_session_reference() {
    let root = std::env::temp_dir().join(format!("ion-retire-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let session = Arc::new(Session::create(root.join("session.db"), None).unwrap());
    let retirement = session.retirement();
    let external = session.clone();
    session.shutdown().await.unwrap();
    drop(session);
    assert!(retirement.try_join().is_none());
    assert_eq!(
        external.reader().unwrap().view().await.unwrap().id,
        external.id()
    );
    drop(external);
    retirement.join().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
