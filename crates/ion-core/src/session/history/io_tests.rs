use std::{
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use futures_util::FutureExt;
use ion_ai::{Content, Message, ModelRef, Role, ToolCall, Usage};
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::{Session, SessionError, execute};
use crate::ToolOccurrence;

struct Fixture {
    root: std::path::PathBuf,
    session: Arc<Session>,
    call: ToolCall,
    parent: ToolOccurrence,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ion-history-io-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let session = Arc::new(Session::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let (turn, _) = session
            .begin_turn(
                "COMMITTED_EVIDENCE".into(),
                ModelRef {
                    provider: "fixture".into(),
                    model: "fixture".into(),
                },
            )
            .unwrap();
        let call = ToolCall {
            id: "history".into(),
            name: super::HISTORY_NAME.into(),
            arguments: json!({"action":"list"}),
            raw_arguments: None,
        };
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(call.clone())],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        let parent = session.tool_occurrence(turn, &call.id).unwrap();
        Self {
            root,
            session,
            call,
            parent,
        }
    }

    fn cleanup(self) {
        drop(self.session);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

// An OS deadline lets even the original executor-blocking defect finish rather
// than deadlocking the test's current-thread runtime.
struct StoreHold {
    release: Option<mpsc::Sender<()>>,
    worker: Option<thread::JoinHandle<bool>>,
}

impl StoreHold {
    async fn new(session: &Arc<Session>) -> Self {
        let session = session.clone();
        let (entered, ready) = oneshot::channel();
        let (release, released) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _store = session.store.lock().unwrap();
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(2)).is_ok()
        });
        ready.await.unwrap();
        Self {
            release: Some(release),
            worker: Some(worker),
        }
    }

    async fn heartbeat(&self) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !self.worker.as_ref().unwrap().is_finished(),
            "Store selection blocked the executor until the OS deadline"
        );
    }
}

impl Drop for StoreHold {
    fn drop(&mut self) {
        let _ = self.release.take().unwrap().send(());
        let released = self.worker.take().unwrap().join().unwrap();
        if !thread::panicking() {
            assert!(released, "Store hold reached its OS deadline");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn store_selection_delay_keeps_executor_responsive_and_issuing_prefix_fixed() {
    let fixture = Fixture::new();
    // These later rows must not advance the issuing assistant's read prefix.
    fixture
        .session
        .admit_user_shell("LATER_PRIVATE_EVIDENCE".into(), true)
        .unwrap();
    let count = fixture.session.entry_count().unwrap();
    let hold = StoreHold::new(&fixture.session).await;
    let mut read = Box::pin(execute(
        &fixture.session,
        &fixture.call,
        fixture.parent,
        CancellationToken::new(),
    ));
    assert!(read.as_mut().now_or_never().is_none());
    hold.heartbeat().await;
    assert!(read.as_mut().now_or_never().is_none());
    drop(hold);
    let output = read.await;
    assert!(!output.is_error, "{output:?}");
    assert_eq!(
        output.value["provenance"]["through_entry"],
        fixture.parent.assistant_entry - 1
    );
    let entries = output.value["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0]["preview"]
            .as_str()
            .unwrap()
            .contains("COMMITTED_EVIDENCE")
    );
    assert!(entries[0]["entry"].as_u64().unwrap() < fixture.parent.assistant_entry);
    assert_eq!(fixture.session.entry_count().unwrap(), count);
    fixture.session.shutdown().await.unwrap();
    fixture.cleanup();
}

#[tokio::test(flavor = "current_thread")]
async fn stop_and_abandoned_history_wait_keep_store_selection_in_shutdown_custody() {
    let fixture = Fixture::new();
    let hold = StoreHold::new(&fixture.session).await;
    let stop = CancellationToken::new();
    let mut read = Box::pin(execute(
        &fixture.session,
        &fixture.call,
        fixture.parent,
        stop.clone(),
    ));
    assert!(read.as_mut().now_or_never().is_none());
    hold.heartbeat().await;
    stop.cancel();
    drop(read);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), fixture.session.shutdown())
            .await
            .is_err(),
        "abandoning the tool wait detached its actual Store selection"
    );
    drop(hold);
    // Cancellation is a tool result, not an unread storage failure.
    fixture.session.shutdown().await.unwrap();
    fixture.cleanup();
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_bad_queries_are_results_but_invalid_store_bounds_remain_failures() {
    for bad_query in [true, false] {
        let mut fixture = Fixture::new();
        let mut parent = fixture.parent;
        if bad_query {
            fixture.call.arguments = json!({"action":"list","limit":0});
        } else {
            // An inconsistent committed bound is a Session invariant failure,
            // not malformed model input that may be discarded with its reply.
            parent.assistant_entry += 100;
        }
        let mut read = Box::pin(execute(
            &fixture.session,
            &fixture.call,
            parent,
            CancellationToken::new(),
        ));
        assert!(read.as_mut().now_or_never().is_none());
        drop(read);
        let shutdown = fixture.session.shutdown().await;
        if bad_query {
            shutdown.unwrap();
        } else {
            assert!(matches!(shutdown, Err(SessionError::Storage(_))));
        }
        fixture.cleanup();
    }
}

#[test]
fn history_has_independent_read_capacity_with_one_occupied_blocking_thread() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let fixture = Fixture::new();
        let (entered, ready) = oneshot::channel();
        let (release, released) = mpsc::channel();
        // A Code Mode guest can occupy this only pool thread while waiting for
        // the native history reply. Exercise that capacity constraint directly.
        let guest = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(2)).is_ok()
        });
        ready.await.unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            execute(
                &fixture.session,
                &fixture.call,
                fixture.parent,
                CancellationToken::new(),
            ),
        )
        .await;
        let _ = release.send(());
        assert!(
            guest.await.unwrap(),
            "occupied pool thread reached its deadline"
        );
        let output = result.expect("history deadlocked on the guest's blocking pool thread");
        assert!(!output.is_error, "{output:?}");
        assert_eq!(output.value["entries"].as_array().unwrap().len(), 1);
        fixture.session.shutdown().await.unwrap();
        fixture.cleanup();
    });
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_actual_history_read_retains_writer_lease_and_shutdown_joins_it() {
    for abandon_session in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.session.path().to_owned();
        // Rollback-journal mode gives a real independent SQLite reader a lock
        // wait, without holding the Session Store or adding a reader test hook.
        fixture
            .session
            .store
            .lock()
            .unwrap()
            .connection
            .pragma_update(None, "journal_mode", "DELETE")
            .unwrap();
        let database = rusqlite::Connection::open(&path).unwrap();
        database.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let mut read = Box::pin(execute(
            &fixture.session,
            &fixture.call,
            fixture.parent,
            CancellationToken::new(),
        ));
        assert!(read.as_mut().now_or_never().is_none());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(read.as_mut().now_or_never().is_none());
        drop(read);
        if abandon_session {
            let Fixture { root, session, .. } = fixture;
            drop(session);
            assert!(matches!(
                Session::open(&path),
                Err(SessionError::AlreadyOpen)
            ));
            database.execute_batch("ROLLBACK").unwrap();
            drop(database);
            // Arc count reaching zero precedes destructor/file-unlock completion.
            // Only reacquiring the physical writer lease proves release.
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
        } else {
            assert!(
                tokio::time::timeout(Duration::from_millis(20), fixture.session.shutdown())
                    .await
                    .is_err(),
                "shutdown returned before the actual SQLite reader joined"
            );
            database.execute_batch("ROLLBACK").unwrap();
            drop(database);
            fixture.session.shutdown().await.unwrap();
            fixture.cleanup();
        }
    }
}
