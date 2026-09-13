//! K4/R1: one session database has one writable owner at a time.
//!
//! Exclusive ownership is what keeps recovery from being reserved twice. The
//! commit-cursor compare-and-set fences a stale canonical *write*, but two
//! processes can both reconstruct state, both reserve recovery and both perform
//! an external action before either discovers its cursor is stale. The owner
//! therefore holds a kernel-held lock for as long as it may write, and releases
//! it last: after admission is closed, canonical writes are fenced and local
//! invocations have joined.

use std::path::Path;

use ion_core::conversation::context::ContextControl;
use ion_core::{
    CloseMode, ConversationId, EntryKind, EntryRequest, Session, SessionError, TaskDriver,
    TaskRegistry,
};
use serde_json::json;

mod support;

use support::TempDb;

const OWNERSHIP_DB: &str = "ION_K4_OWNERSHIP_DB";
const OWNERSHIP_EXPECT: &str = "ION_K4_OWNERSHIP_EXPECT";

fn note(conversation_id: ConversationId, text: &str) -> EntryRequest {
    EntryRequest {
        conversation_id,
        kind: EntryKind::new("note").expect("entry kind"),
        data: json!({"text": text}),
        projection: Vec::new(),
        context: ContextControl::none(),
    }
}

#[test]
fn a_live_owner_refuses_a_second_writable_open() {
    let db = TempDb::new("ownership-live");
    let owner = Session::create(db.path()).expect("create");

    let refused = Session::open(db.path()).expect_err("a live owner is exclusive");
    assert!(
        matches!(refused, SessionError::SessionInUse(ref path) if path == db.path()),
        "unexpected error: {refused:?}"
    );

    drop(owner);
    Session::open(db.path()).expect("ownership is available once the owner goes away");
}

#[test]
fn the_refused_process_observes_and_changes_nothing() {
    let db = TempDb::new("ownership-no-work");
    let mut owner = Session::create(db.path()).expect("create");
    let root = owner.root_conversation();
    owner.append_entry(note(root, "owned")).expect("commit");
    let committed = owner.summary().last_commit;

    assert!(Session::open(db.path()).is_err());

    // The refusal is not a read that half-succeeded: no session was handed out,
    // and the durable cursor and transcript are unchanged.
    assert_eq!(owner.summary().last_commit, committed);
    assert_eq!(owner.snapshot().entries.len(), 1);
}

#[tokio::test]
async fn a_closed_driver_releases_ownership() {
    let db = TempDb::new("ownership-close");
    let driver =
        TaskDriver::create(db.path(), TaskRegistry::new()).expect("create over the driver");
    assert!(Session::open(db.path()).is_err(), "the driver owns it");

    driver.close(CloseMode::Graceful).await;

    let mut next = Session::open(db.path()).expect("ownership after a clean close");
    let root = next.root_conversation();
    next.append_entry(note(root, "new owner"))
        .expect("the next owner can commit");
}

#[tokio::test]
async fn a_fault_closed_driver_also_releases_ownership() {
    let db = TempDb::new("ownership-fault-close");
    let driver =
        TaskDriver::create(db.path(), TaskRegistry::new()).expect("create over the driver");
    assert!(Session::open(db.path()).is_err(), "the driver owns it");

    driver.close(CloseMode::Fault).await;
    Session::open(db.path()).expect("ownership after a fault close");
}

#[cfg(unix)]
#[test]
fn a_child_process_cannot_take_a_live_session_and_can_after_release() {
    let db = TempDb::new("ownership-child");
    let owner = Session::create(db.path()).expect("create");

    run_child(db.path(), "refused");
    assert!(
        Session::open(db.path()).is_err(),
        "the parent still owns the session"
    );

    drop(owner);
    run_child(db.path(), "granted");
}

/// Runs only inside the child process spawned by the test above. A child that
/// expects to be refused must fail to open at all; a child that expects to be
/// granted the session must be able to commit with it.
#[test]
fn ownership_child() {
    let Some(path) = std::env::var_os(OWNERSHIP_DB) else {
        return;
    };
    let expectation = std::env::var(OWNERSHIP_EXPECT).expect("expectation");
    match Session::open(Path::new(&path)) {
        Err(SessionError::SessionInUse(_)) => assert_eq!(expectation, "refused"),
        Ok(mut session) => {
            assert_eq!(
                expectation, "granted",
                "the child was not supposed to own it"
            );
            let root = session.root_conversation();
            session
                .append_entry(note(root, "from the child owner"))
                .expect("the new owner can commit");
        }
        Err(error) => panic!("child expected {expectation}, got {error}"),
    }
}

fn run_child(path: &Path, expectation: &str) {
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "ownership_child", "--nocapture"])
        .env(OWNERSHIP_DB, path)
        .env(OWNERSHIP_EXPECT, expectation)
        .status()
        .expect("spawn ownership child");
    assert!(status.success(), "child {expectation} failed: {status:?}");
}

/// Reconstruction reads one transaction, so a writer committing behind it is
/// never observed half-applied.
///
/// The interfering writer commits raw SQL on purpose: a second *store* owner is
/// now refused, so bypassing ownership is the only way to have a concurrent
/// committer. It copies a real entry row and advances the durable cursor in one
/// transaction, which is the shape the store writes, so
/// "no visible entry is newer than the committed cursor" holds for every
/// consistent snapshot and fails for a torn one. A passing run is evidence that
/// no tear was observed, not proof that none is possible.
#[test]
fn reconstruction_never_observes_a_half_applied_commit() {
    let db = TempDb::new("ownership-snapshot");
    let mut seed = Session::create(db.path()).expect("create");
    let root = seed.root_conversation();
    seed.append_entry(note(root, "seed")).expect("commit");
    drop(seed);

    let path = db.path().to_path_buf();
    let writer_path = path.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_stop = stop.clone();
    let writer = std::thread::spawn(move || {
        let mut connection = rusqlite::Connection::open(&writer_path).expect("raw writer");
        connection
            .pragma_update(None, "busy_timeout", 5_000)
            .expect("busy timeout");
        let mut seq = 100_i64;
        while !writer_stop.load(std::sync::atomic::Ordering::Relaxed) {
            let transaction = connection.transaction().expect("begin");
            transaction
                .execute(
                    "INSERT INTO entries (id, conversation_id, kind, data, projection, context)
                     SELECT ?1, conversation_id, kind, data, projection, context
                     FROM entries WHERE id = (SELECT MIN(id) FROM entries)",
                    rusqlite::params![seq],
                )
                .expect("insert");
            transaction
                .execute(
                    "UPDATE session_meta SET last_seq = ?1, last_commit = ?1 WHERE id = 1",
                    rusqlite::params![seq],
                )
                .expect("advance cursor");
            transaction.commit().expect("commit");
            seq += 1;
            // Leave room between commits so the reader actually interleaves.
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        seq
    });

    let mut reads = 0_u64;
    let mut previous = 0_i64;
    let mut unexpected = Vec::new();
    for _ in 0..150 {
        // Sessions are never held across iterations, so the writer can commit
        // between two loads.
        match Session::open(&path) {
            Ok(session) => {
                let summary = session.summary();
                let newest = session
                    .snapshot()
                    .entries
                    .iter()
                    .map(|entry| entry.id.get())
                    .max()
                    .unwrap_or_default();
                assert!(
                    newest <= summary.last_commit.get(),
                    "a torn read saw entry {newest} with committed cursor {}",
                    summary.last_commit.get()
                );
                assert!(
                    summary.last_commit.get() >= previous,
                    "the durable cursor went backwards: {} < {previous}",
                    summary.last_commit.get()
                );
                previous = summary.last_commit.get();
                reads += 1;
            }
            Err(error) => {
                let text = error.to_string();
                if !text.contains("locked") && !text.contains("busy") {
                    unexpected.push(text);
                }
            }
        }
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let committed = writer.join().expect("writer thread");
    assert!(
        reads > 0,
        "the reader must have observed at least one snapshot"
    );
    assert!(committed > 100, "the writer must have committed repeatedly");
    assert!(
        unexpected.is_empty(),
        "reconstruction failed for reasons other than lock contention: {unexpected:?}"
    );

    let mut final_session = Session::open(&path).expect("final open");
    let root = final_session.root_conversation();
    final_session
        .append_entry(note(root, "after the writer stopped"))
        .expect("ownership is free once the writer stopped");
}
