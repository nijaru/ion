//! K4 persistence: a real per-session database, reopened from disk.
//!
//! These tests exercise the durable boundary rather than the resident one: what
//! survives a close, what a second authority can do, and what opening does.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ion_core::conversation::context::ContextControl;
use ion_core::{
    CloseMode, ConversationSpec, EntryKind, EntryRequest, InputBody, InputMode, InputRequest,
    InputSender, PlannedEntry, PlannedTask, RequestKey, RunningTask, Session, SessionError,
    TaskCompletion, TaskContext, TaskDriver, TaskFuture, TaskId, TaskKind, TaskKindName, TaskPlan,
    TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::json;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

/// Removes the database and its WAL sidecars when the test ends.
struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ion-k4-{name}-{}.sqlite",
            ion_core::SessionId::new()
        ));
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

/// Settles with a plan that leaves one scoped successor, so the turn stays live.
struct ScopedSuccessor;

impl TaskKind for ScopedSuccessor {
    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            context
                .checkpoint(Some(json!({"phase": "before-finalization"})), None)
                .await?;
            let mut plan = TaskPlan::new();
            plan.append_entry(PlannedEntry {
                conversation_id: task.conversation_id,
                kind: EntryKind::new("assistant").expect("entry kind"),
                data: json!({"text": "dispatching"}),
                projection: Vec::new(),
                context: ContextControl::none(),
            });
            plan.create_task(PlannedTask {
                conversation_id: task.conversation_id,
                kind: kind("tool"),
                schema_version: 1,
                input: json!({"call": "scoped"}),
                dependencies: Vec::new(),
                background: false,
            });
            Ok(TaskCompletion::completed(json!("dispatched")).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

/// Settles with a plan whose only successor is background, so the turn ends.
struct BackgroundSuccessor;

impl TaskKind for BackgroundSuccessor {
    fn execute<'a>(&'a self, task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            plan.create_task(PlannedTask {
                conversation_id: task.conversation_id,
                kind: kind("worker"),
                schema_version: 1,
                input: json!({"task": "background"}),
                dependencies: Vec::new(),
                background: true,
            });
            Ok(TaskCompletion::completed(json!("spawned")).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

/// The foreground turn root a specific conversation currently holds.
async fn foreground(
    driver: &TaskDriver,
    conversation_id: ion_core::ConversationId,
) -> Option<TaskId> {
    driver
        .snapshot()
        .await
        .conversations
        .into_iter()
        .find(|conversation| conversation.id == conversation_id)
        .expect("conversation exists")
        .foreground_turn
}

fn registry() -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    registry
        .register(kind("scoped"), 1, Arc::new(ScopedSuccessor))
        .expect("register");
    registry
        .register(kind("background"), 1, Arc::new(BackgroundSuccessor))
        .expect("register");
    registry
}

/// Commit a broad, representative write set and return the session together
/// with the receipts a reopened session must reproduce.
async fn build_session(path: &Path) -> (TaskDriver, TaskId) {
    let mut session = Session::create(path).expect("create");
    let root = session.root_conversation();

    let first = session
        .append_entry(EntryRequest {
            conversation_id: root,
            kind: EntryKind::new("user").expect("entry kind"),
            data: json!({"text": "hello"}),
            projection: Vec::new(),
            context: ContextControl::none(),
        })
        .expect("entry");
    session
        .append_entry(EntryRequest {
            conversation_id: root,
            kind: EntryKind::new("assistant").expect("entry kind"),
            data: json!({"text": "hi"}),
            projection: Vec::new(),
            context: ContextControl::none(),
        })
        .expect("entry");
    let fork = session
        .create_conversation(ConversationSpec {
            parent: Some(ion_core::HistoryParent {
                conversation_id: root,
                at: first.entry_id,
            }),
            owner_task: None,
        })
        .expect("fork")
        .conversation_id;
    let admitted = session
        .queue_input(InputRequest {
            target: root,
            sender: InputSender::User,
            mode: InputMode::Submit,
            request_key: Some(RequestKey::new("durable-key").expect("key")),
            body: InputBody::Text("go".to_owned()),
        })
        .expect("input")
        .input_id;

    let pending = session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind("tool"),
            schema_version: 1,
            input: json!({"call": "pending"}),
            dependencies: Vec::new(),
        })
        .expect("task")
        .task_id;
    // A second task owned by nothing, used to prove dependency rows survive.
    session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind("join"),
            schema_version: 1,
            input: json!({"waiting": true}),
            dependencies: vec![pending],
        })
        .expect("dependent task");
    // A conversation owned by a task, written as two mutations in one batch.
    session
        .create_conversation(ConversationSpec {
            parent: None,
            owner_task: Some(pending),
        })
        .expect("owned conversation");

    let driver = TaskDriver::new(session, registry());

    // A turn that keeps its slot because a scoped successor remains.
    let turn = driver
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: kind("scoped"),
            schema_version: 1,
            input: json!({"prompt": "first"}),
            dependencies: Vec::new(),
        })
        .await
        .expect("turn");
    driver.drive_task(turn.task_id).await.expect("drive turn");
    assert_eq!(
        foreground(&driver, root).await,
        Some(turn.task_id),
        "a scoped successor keeps the turn slot open"
    );

    // A second turn, on the fork, whose only successor is background: creating
    // it opens the slot and finalization releases it again.
    let background = driver
        .create_turn(TaskRequest {
            conversation_id: fork,
            kind: kind("background"),
            schema_version: 1,
            input: json!({"prompt": "second"}),
            dependencies: Vec::new(),
        })
        .await
        .expect("turn");
    assert_eq!(foreground(&driver, fork).await, Some(background.task_id));
    driver
        .drive_task(background.task_id)
        .await
        .expect("drive turn");
    assert_eq!(foreground(&driver, fork).await, None);
    assert_eq!(
        foreground(&driver, root).await,
        Some(turn.task_id),
        "the unrelated turn is unaffected"
    );

    // A durable cancellation mark on a still-pending task.
    let cancelled = driver.cancel_task(pending).await.expect("cancel pending");
    assert!(cancelled.changed);
    driver
        .assign_input(admitted, pending)
        .await
        .expect("assign input");

    (driver, turn.task_id)
}

#[tokio::test]
async fn reopening_reconstructs_entries_inputs_tasks_and_turns() {
    let db = TempDb::new("roundtrip");
    let (driver, _turn) = build_session(db.path()).await;
    let before = driver.snapshot().await;
    let summary = driver.summary().await;
    let observations = driver.observations_after(None).await;
    assert!(summary.entries >= 3);
    assert!(!observations.events.is_empty());
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    let mut reopened = Session::open(db.path()).expect("reopen");
    assert_eq!(reopened.summary(), summary);
    assert_eq!(reopened.snapshot(), before);
    assert_eq!(reopened.session_id(), summary.session_id);
    assert_eq!(reopened.root_conversation(), summary.root_conversation);

    // The durable request key and admission commit reconstruct duplicate-input
    // receipts without re-admitting the input.
    let input = reopened.snapshot().inputs[0].clone();
    assert_eq!(input.disposition, before.inputs[0].disposition);
    let key = input.request_key.clone().expect("request key survived");
    let replay = reopened
        .queue_input(InputRequest {
            target: input.target,
            sender: input.sender,
            mode: input.mode,
            request_key: Some(key),
            body: input.body,
        })
        .expect("replay");
    assert!(replay.replayed);
    assert_eq!(replay.input_id, input.id);
    assert_eq!(reopened.summary(), summary, "replay commits nothing new");

    // Opening a second authority over the same file is a read; it starts no work.
    let second = Session::open(db.path()).expect("second open");
    assert_eq!(second.snapshot(), before);
}

#[tokio::test]
async fn a_driver_creates_and_opens_an_on_disk_session() {
    let db = TempDb::new("driver-open");
    let driver = TaskDriver::create(db.path(), registry()).expect("create over the driver");
    let root = driver.snapshot().await.root_conversation;
    let turn = driver
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: kind("scoped"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .await
        .expect("turn");
    driver.drive_task(turn.task_id).await.expect("drive");
    let before = driver.snapshot().await;
    let summary = driver.summary().await;
    assert_eq!(
        foreground(&driver, root).await,
        Some(turn.task_id),
        "the scoped successor still holds the slot"
    );
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    // The client can reopen through the driver, and opening reads only: the
    // pending successor is not driven and the slot is unchanged.
    let reopened = TaskDriver::open(db.path(), registry()).expect("open over the driver");
    assert_eq!(reopened.snapshot().await, before);
    assert_eq!(reopened.summary().await, summary);
    assert_eq!(foreground(&reopened, root).await, Some(turn.task_id));
    assert_eq!(
        reopened
            .snapshot()
            .await
            .tasks
            .iter()
            .filter(|task| matches!(task.status, TaskStatus::Pending))
            .count(),
        1,
        "opening starts no work"
    );
}

/// A handler that panics leaves the task durably running, which is the state a
/// reopened process has to find and explicitly recover.
struct Panicking;

impl TaskKind for Panicking {
    fn execute<'a>(&'a self, _task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move { panic!("process died before settling") })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

#[tokio::test]
async fn reopening_leaves_running_tasks_for_explicit_recovery() {
    let db = TempDb::new("running");
    let path = db.path().to_path_buf();

    let mut session = Session::create(&path).expect("create");
    let root = session.root_conversation();
    let task = session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind("panicking"),
            schema_version: 1,
            input: json!({"prompt": "interrupted"}),
            dependencies: Vec::new(),
        })
        .expect("task")
        .task_id;
    let mut registry = TaskRegistry::new();
    registry
        .register(kind("panicking"), 1, Arc::new(Panicking))
        .expect("register");
    let driver = TaskDriver::new(session, registry);
    let outcome = driver.drive_task(task).await.expect("drive");
    assert!(matches!(outcome, ion_core::DriveOutcome::Interrupted(_)));

    let committed = driver.summary().await.last_commit;
    let before = driver.snapshot().await;
    let running = before
        .tasks
        .iter()
        .find(|record| record.id == task)
        .expect("task record");
    assert!(matches!(running.status, TaskStatus::Running));
    let generation = running.generation;
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    let reopened = Session::open(&path).expect("reopen");
    let record = reopened
        .snapshot()
        .tasks
        .into_iter()
        .find(|record| record.id == task)
        .expect("task survived");
    assert!(matches!(record.status, TaskStatus::Running));
    assert_eq!(record.generation, generation);
    assert_eq!(
        record.invocation.expect("invocation").generation,
        generation
    );
    assert_eq!(
        reopened.summary().last_commit,
        committed,
        "opening commits nothing and starts no work"
    );
}

#[tokio::test]
async fn a_stale_writer_authority_is_fenced() {
    let db = TempDb::new("fence");
    let path = db.path().to_path_buf();
    let _original = Session::create(&path).expect("create");

    let mut first = Session::open(&path).expect("first");
    let mut second = Session::open(&path).expect("second");
    let root = first.root_conversation();
    let entry = |text: &str| EntryRequest {
        conversation_id: root,
        kind: EntryKind::new("note").expect("entry kind"),
        data: json!({"text": text}),
        projection: Vec::new(),
        context: ContextControl::none(),
    };

    first.append_entry(entry("from first")).expect("commit");
    let error = second
        .append_entry(entry("from second"))
        .expect_err("a stale authority must be rejected");
    assert!(matches!(error, SessionError::Persistence(_)));
    // The fenced authority is closed rather than left usable.
    assert!(matches!(
        second.append_entry(entry("again")),
        Err(SessionError::Closed)
    ));

    let durable = Session::open(&path).expect("reopen");
    let texts: Vec<_> = durable
        .snapshot()
        .entries
        .iter()
        .map(|entry| entry.data["text"].clone())
        .collect();
    assert_eq!(texts, vec![json!("from first")]);
}

/// Environment variable naming the database a crash child should write to.
const CRASH_DB: &str = "ION_K4_CRASH_DB";

/// Payloads the crash child commits and the parent then looks for.
const CRASH_PAYLOADS: [&str; 3] = [
    "committed-before-crash-0",
    "committed-before-crash-1",
    "committed-before-crash-2",
];

/// Run only inside the child process spawned by
/// `committed_writes_survive_process_death`. It commits, then dies without
/// unwinding, closing or checkpointing anything.
#[test]
fn crash_child_commits_then_dies() {
    let Some(path) = std::env::var_os(CRASH_DB) else {
        return;
    };
    let mut session = Session::create(&path).expect("create");
    let root = session.root_conversation();
    for payload in CRASH_PAYLOADS {
        session
            .append_entry(EntryRequest {
                conversation_id: root,
                kind: EntryKind::new("note").expect("entry kind"),
                data: json!({"payload": payload}),
                projection: Vec::new(),
                context: ContextControl::none(),
            })
            .expect("commit");
    }
    // No destructors, no clean close, no WAL checkpoint: the process simply
    // stops. Everything already committed must still be there.
    std::process::abort();
}

#[cfg(unix)]
#[test]
fn committed_writes_survive_process_death() {
    use std::os::unix::process::ExitStatusExt;

    let db = TempDb::new("crash");
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "crash_child_commits_then_dies", "--nocapture"])
        .env(CRASH_DB, db.path())
        .status()
        .expect("spawn crash child");

    assert_eq!(
        status.signal(),
        Some(6),
        "the child must die on SIGABRT, not exit or panic: {status:?}"
    );

    let reopened = Session::open(db.path()).expect("reopen after process death");
    let payloads: Vec<_> = reopened
        .snapshot()
        .entries
        .iter()
        .map(|entry| entry.data["payload"].clone())
        .collect();
    assert_eq!(
        payloads,
        CRASH_PAYLOADS.map(|payload| json!(payload)).to_vec(),
        "every commit acknowledged before the crash must be durable"
    );
    assert!(reopened.summary().last_commit.local_seq().get() > 0);

    // Recovery is idempotent: a second open sees exactly the same history.
    let again = Session::open(db.path()).expect("second reopen");
    assert_eq!(again.snapshot(), reopened.snapshot());
}
