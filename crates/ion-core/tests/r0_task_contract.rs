pub mod r0_support;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use r0_support::{
    AbortPlan, BoxFuture, Completion, PrototypeTaskStore, RunningTask, StoreError, TaskContext,
    TaskError, TaskKind, TaskRegistry, TaskStatus, TerminalPlan, abort_task, create_task,
    execute_task, execute_task_with_cancellation, mark_cancel, recover_task, shared_store, task,
};
use serde::{Deserialize, Serialize};
use tempfile::tempdir;
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SimpleInput {
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum SimpleCheckpoint {
    Started,
}

struct ImmediateKind;

impl TaskKind for ImmediateKind {
    const KIND: &'static str = "r0.immediate";

    type Input = SimpleInput;
    type Checkpoint = SimpleCheckpoint;
    type Completed = String;
    type Failure = String;
    type Aborted = String;

    fn execute<'a>(
        &'a self,
        task: RunningTask<Self::Input, Self::Checkpoint>,
        context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    > {
        Box::pin(async move {
            assert!(task.id.0 > 0);
            assert_eq!(task.generation, 1);
            context.commit(|tx| tx.checkpoint(SimpleCheckpoint::Started))?;
            Ok(TerminalPlan::completed(format!("done:{}", task.input.text)))
        })
    }

    fn recover<'a>(
        &'a self,
        task: RunningTask<Self::Input, Self::Checkpoint>,
        _context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    > {
        Box::pin(async move {
            match task.checkpoint {
                Some(SimpleCheckpoint::Started) => {
                    Ok(TerminalPlan::completed("recovered".to_owned()))
                }
                None => Ok(TerminalPlan::failed("missing checkpoint".to_owned())),
            }
        })
    }

    fn abort<'a>(
        &'a self,
        _task: RunningTask<Self::Input, Self::Checkpoint>,
        _context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<'a, Result<AbortPlan<Self::Checkpoint, Self::Aborted>, TaskError>> {
        Box::pin(async { Ok(AbortPlan::new("aborted".to_owned())) })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum BlockingCheckpoint {
    Waiting,
}

struct BlockingKind {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

impl TaskKind for BlockingKind {
    const KIND: &'static str = "r0.blocking";

    type Input = SimpleInput;
    type Checkpoint = BlockingCheckpoint;
    type Completed = String;
    type Failure = String;
    type Aborted = String;

    fn execute<'a>(
        &'a self,
        _task: RunningTask<Self::Input, Self::Checkpoint>,
        context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    > {
        let started = Arc::clone(&self.started);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            context.commit(|tx| tx.checkpoint(BlockingCheckpoint::Waiting))?;
            started.notify_one();
            tokio::select! {
                () = context.cancelled() => Ok(TerminalPlan::completed("cancel-observed".to_owned())),
                () = release.notified() => Ok(TerminalPlan::completed("normal".to_owned())),
            }
        })
    }

    fn recover<'a>(
        &'a self,
        _task: RunningTask<Self::Input, Self::Checkpoint>,
        _context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    > {
        Box::pin(async { Ok(TerminalPlan::completed("recovered".to_owned())) })
    }

    fn abort<'a>(
        &'a self,
        task: RunningTask<Self::Input, Self::Checkpoint>,
        _context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<'a, Result<AbortPlan<Self::Checkpoint, Self::Aborted>, TaskError>> {
        Box::pin(async move {
            assert_eq!(task.checkpoint, Some(BlockingCheckpoint::Waiting));
            Ok(AbortPlan::new("abort-cleanup".to_owned()))
        })
    }
}

struct LeakyContextKind {
    context_tx: std::sync::Mutex<Option<oneshot::Sender<TaskContext<BlockingCheckpoint>>>>,
    release: Arc<Notify>,
}

impl TaskKind for LeakyContextKind {
    const KIND: &'static str = "r0.leaky-context";

    type Input = SimpleInput;
    type Checkpoint = BlockingCheckpoint;
    type Completed = String;
    type Failure = String;
    type Aborted = String;

    fn execute<'a>(
        &'a self,
        _task: RunningTask<Self::Input, Self::Checkpoint>,
        context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    > {
        let release = Arc::clone(&self.release);
        let sender = self.context_tx.lock().expect("context sender mutex").take();
        Box::pin(async move {
            context.commit(|tx| tx.checkpoint(BlockingCheckpoint::Waiting))?;
            if let Some(sender) = sender {
                let _ = sender.send(context.clone());
            }
            release.notified().await;
            Ok(TerminalPlan::completed("normal".to_owned()))
        })
    }

    fn recover<'a>(
        &'a self,
        _task: RunningTask<Self::Input, Self::Checkpoint>,
        _context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    > {
        Box::pin(async { Ok(TerminalPlan::completed("recovered".to_owned())) })
    }

    fn abort<'a>(
        &'a self,
        _task: RunningTask<Self::Input, Self::Checkpoint>,
        _context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<'a, Result<AbortPlan<Self::Checkpoint, Self::Aborted>, TaskError>> {
        Box::pin(async { Ok(AbortPlan::new("aborted".to_owned())) })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum CrashCheckpoint {
    DurableBeforeCrash,
}

struct CrashCheckpointKind;

impl TaskKind for CrashCheckpointKind {
    const KIND: &'static str = "r0.crash-checkpoint";

    type Input = SimpleInput;
    type Checkpoint = CrashCheckpoint;
    type Completed = String;
    type Failure = String;
    type Aborted = String;

    fn execute<'a>(
        &'a self,
        task: RunningTask<Self::Input, Self::Checkpoint>,
        context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    > {
        Box::pin(async move {
            context.commit(|tx| tx.checkpoint(CrashCheckpoint::DurableBeforeCrash))?;
            append_witness(
                std::path::Path::new(&task.input.text),
                "checkpoint committed",
            );
            std::process::exit(83)
        })
    }

    fn recover<'a>(
        &'a self,
        task: RunningTask<Self::Input, Self::Checkpoint>,
        _context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    > {
        Box::pin(async move {
            assert_eq!(task.checkpoint, Some(CrashCheckpoint::DurableBeforeCrash));
            assert!(task.generation > 1);
            Ok(TerminalPlan::completed("recovered-after-crash".to_owned()))
        })
    }

    fn abort<'a>(
        &'a self,
        _task: RunningTask<Self::Input, Self::Checkpoint>,
        _context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<'a, Result<AbortPlan<Self::Checkpoint, Self::Aborted>, TaskError>> {
        Box::pin(async { Ok(AbortPlan::new("aborted".to_owned())) })
    }
}

struct PanicThenRecoverKind;

impl TaskKind for PanicThenRecoverKind {
    const KIND: &'static str = "r0.panic-recover";

    type Input = SimpleInput;
    type Checkpoint = SimpleCheckpoint;
    type Completed = String;
    type Failure = String;
    type Aborted = String;

    fn execute<'a>(
        &'a self,
        _task: RunningTask<Self::Input, Self::Checkpoint>,
        context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    > {
        Box::pin(async move {
            context.commit(|tx| tx.checkpoint(SimpleCheckpoint::Started))?;
            panic!("simulated task implementation panic")
        })
    }

    fn recover<'a>(
        &'a self,
        task: RunningTask<Self::Input, Self::Checkpoint>,
        _context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    > {
        Box::pin(async move {
            assert_eq!(task.checkpoint, Some(SimpleCheckpoint::Started));
            Ok(TerminalPlan::completed("panic-recovered".to_owned()))
        })
    }

    fn abort<'a>(
        &'a self,
        _task: RunningTask<Self::Input, Self::Checkpoint>,
        _context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<'a, Result<AbortPlan<Self::Checkpoint, Self::Aborted>, TaskError>> {
        Box::pin(async { Ok(AbortPlan::new("aborted".to_owned())) })
    }
}

fn new_store(path: &std::path::Path) -> r0_support::SharedStore {
    shared_store(PrototypeTaskStore::open(path).expect("open prototype store"))
}

fn terminal_value(status: TaskStatus) -> (&'static str, serde_json::Value) {
    match status {
        TaskStatus::Terminal { kind, value } => {
            let kind = match kind.as_str() {
                "completed" => "completed",
                "failed" => "failed",
                "aborted" => "aborted",
                other => panic!("unexpected terminal kind {other}"),
            };
            (kind, value)
        }
        other => panic!("expected terminal task, got {other:?}"),
    }
}

#[tokio::test]
async fn typed_task_kind_survives_registry_erasure_and_terminal_commit() {
    let directory = tempdir().expect("tempdir");
    let store = new_store(&directory.path().join("task.sqlite"));
    let task_id = create_task::<ImmediateKind>(
        &store,
        &SimpleInput {
            text: "hello".to_owned(),
        },
    )
    .expect("create task");
    let mut registry = TaskRegistry::default();
    registry.register(ImmediateKind);

    execute_task(&registry, Arc::clone(&store), task_id)
        .await
        .expect("execute");

    let stored = task(&store, task_id).expect("stored task");
    assert_eq!(stored.checkpoint, Some(serde_json::json!("Started")));
    let (kind, value) = terminal_value(stored.status);
    assert_eq!(kind, "completed");
    assert_eq!(value, serde_json::json!("done:hello"));
}

#[tokio::test]
async fn settlement_before_cancel_wins() {
    let directory = tempdir().expect("tempdir");
    let store = new_store(&directory.path().join("settle.sqlite"));
    let task_id = create_task::<ImmediateKind>(
        &store,
        &SimpleInput {
            text: "first".to_owned(),
        },
    )
    .expect("create task");
    let mut registry = TaskRegistry::default();
    registry.register(ImmediateKind);

    execute_task(&registry, Arc::clone(&store), task_id)
        .await
        .expect("execute");
    assert!(!mark_cancel(&store, task_id).expect("late cancel"));
    let (kind, value) = terminal_value(task(&store, task_id).expect("task").status);
    assert_eq!(
        (kind, value),
        ("completed", serde_json::json!("done:first"))
    );
}

#[tokio::test]
async fn cancel_before_settlement_fences_normal_completion_then_runs_fresh_abort() {
    let directory = tempdir().expect("tempdir");
    let store = new_store(&directory.path().join("cancel.sqlite"));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let cancellation = CancellationToken::new();
    let task_id = create_task::<BlockingKind>(
        &store,
        &SimpleInput {
            text: "blocking".to_owned(),
        },
    )
    .expect("create task");
    let mut registry = TaskRegistry::default();
    registry.register(BlockingKind {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    });
    let registry = Arc::new(registry);

    let running = tokio::spawn({
        let registry = Arc::clone(&registry);
        let store = Arc::clone(&store);
        let cancellation = cancellation.clone();
        async move { execute_task_with_cancellation(&registry, store, task_id, cancellation).await }
    });
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("task starts");
    assert!(mark_cancel(&store, task_id).expect("cancel mark"));
    let generation_before_abort = task(&store, task_id).expect("task").generation;
    cancellation.cancel();
    let completion = running.await.expect("driver join");
    assert!(matches!(
        completion,
        Err(TaskError::Store(StoreError::Cancelled))
    ));

    abort_task(&registry, Arc::clone(&store), task_id)
        .await
        .expect("fresh abort");
    let stored = task(&store, task_id).expect("stored task");
    assert!(stored.generation > generation_before_abort);
    let (kind, value) = terminal_value(stored.status);
    assert_eq!(
        (kind, value),
        ("aborted", serde_json::json!("abort-cleanup"))
    );
}

#[tokio::test]
async fn leaked_old_invocation_context_is_generation_fenced_after_abort_reservation() {
    let directory = tempdir().expect("tempdir");
    let store = new_store(&directory.path().join("stale.sqlite"));
    let release = Arc::new(Notify::new());
    let (context_tx, context_rx) = oneshot::channel();
    let task_id = create_task::<LeakyContextKind>(
        &store,
        &SimpleInput {
            text: "leak".to_owned(),
        },
    )
    .expect("create task");
    let mut registry = TaskRegistry::default();
    registry.register(LeakyContextKind {
        context_tx: std::sync::Mutex::new(Some(context_tx)),
        release: Arc::clone(&release),
    });
    let registry = Arc::new(registry);
    let running = tokio::spawn({
        let registry = Arc::clone(&registry);
        let store = Arc::clone(&store);
        async move { execute_task(&registry, store, task_id).await }
    });
    let leaked = tokio::time::timeout(Duration::from_secs(1), context_rx)
        .await
        .expect("context capture")
        .expect("context sender");
    assert!(mark_cancel(&store, task_id).expect("cancel mark"));
    release.notify_one();
    assert!(matches!(
        running.await.expect("driver join"),
        Err(TaskError::Store(StoreError::Cancelled))
    ));

    {
        let mut store_guard = store.lock().expect("store mutex");
        store_guard.reserve_abort(task_id).expect("reserve abort");
    }
    let late = leaked.commit(|tx| tx.checkpoint(BlockingCheckpoint::Waiting));
    assert!(matches!(
        late,
        Err(TaskError::Store(StoreError::StaleInvocation))
    ));
}

#[tokio::test]
async fn cancelling_a_waiter_does_not_cancel_the_durable_task() {
    let directory = tempdir().expect("tempdir");
    let store = new_store(&directory.path().join("waiter.sqlite"));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let terminal_wake = Arc::new(Notify::new());
    let task_id = create_task::<BlockingKind>(
        &store,
        &SimpleInput {
            text: "wait".to_owned(),
        },
    )
    .expect("create task");
    let mut registry = TaskRegistry::default();
    registry.register(BlockingKind {
        started: Arc::clone(&started),
        release: Arc::clone(&release),
    });
    let registry = Arc::new(registry);

    let running = tokio::spawn({
        let registry = Arc::clone(&registry);
        let store = Arc::clone(&store);
        let terminal_wake = Arc::clone(&terminal_wake);
        async move {
            let result = execute_task(&registry, store, task_id).await;
            terminal_wake.notify_waiters();
            result
        }
    });
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("task starts");

    let waiter = tokio::spawn({
        let store = Arc::clone(&store);
        let terminal_wake = Arc::clone(&terminal_wake);
        async move {
            loop {
                if matches!(
                    task(&store, task_id).expect("task").status,
                    TaskStatus::Terminal { .. }
                ) {
                    return;
                }
                terminal_wake.notified().await;
            }
        }
    });
    waiter.abort();
    assert!(waiter.await.expect_err("waiter cancelled").is_cancelled());
    assert!(matches!(
        task(&store, task_id).expect("task").status,
        TaskStatus::Running
    ));

    release.notify_one();
    running
        .await
        .expect("driver join")
        .expect("task still completes");
    assert!(matches!(
        task(&store, task_id).expect("task").status,
        TaskStatus::Terminal { .. }
    ));
}

#[tokio::test]
async fn task_implementation_panic_leaves_recoverable_running_state() {
    let directory = tempdir().expect("tempdir");
    let store = new_store(&directory.path().join("panic.sqlite"));
    let task_id = create_task::<PanicThenRecoverKind>(
        &store,
        &SimpleInput {
            text: "panic".to_owned(),
        },
    )
    .expect("create task");
    let mut registry = TaskRegistry::default();
    registry.register(PanicThenRecoverKind);
    let registry = Arc::new(registry);

    let running = tokio::spawn({
        let registry = Arc::clone(&registry);
        let store = Arc::clone(&store);
        async move { execute_task(&registry, store, task_id).await }
    });
    assert!(running.await.expect_err("task panics").is_panic());
    let after_panic = task(&store, task_id).expect("task after panic");
    assert_eq!(after_panic.checkpoint, Some(serde_json::json!("Started")));
    assert!(matches!(after_panic.status, TaskStatus::Running));

    recover_task(&registry, Arc::clone(&store), task_id)
        .await
        .expect("recover after panic");
    let (kind, value) = terminal_value(task(&store, task_id).expect("task").status);
    assert_eq!(
        (kind, value),
        ("completed", serde_json::json!("panic-recovered"))
    );
}

fn append_witness(path: &std::path::Path, text: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open witness");
    writeln!(file, "{text}").expect("write witness");
    file.sync_all().expect("sync witness");
}

#[tokio::test]
async fn r0_crash_child_checkpoint() {
    let Some(db) = std::env::var_os("ION_R0_TASK_DB") else {
        return;
    };
    if std::env::var("ION_R0_TASK_CHILD").ok().as_deref() != Some("checkpoint") {
        return;
    }
    let task_id = r0_support::TaskId(
        std::env::var("ION_R0_TASK_ID")
            .expect("task id")
            .parse()
            .expect("numeric task id"),
    );
    let store = new_store(std::path::Path::new(&db));
    let mut registry = TaskRegistry::default();
    registry.register(CrashCheckpointKind);
    execute_task(&registry, store, task_id)
        .await
        .expect("child execution exits before returning");
}

#[tokio::test]
async fn abrupt_process_loss_recovers_from_latest_durable_checkpoint() {
    let directory = tempdir().expect("tempdir");
    let db = directory.path().join("crash.sqlite");
    let witness = directory.path().join("crash.witness");
    let store = new_store(&db);
    let task_id = create_task::<CrashCheckpointKind>(
        &store,
        &SimpleInput {
            text: witness.to_string_lossy().into_owned(),
        },
    )
    .expect("create task");
    drop(store);

    let executable = std::env::current_exe().expect("test executable");
    let status = Command::new(executable)
        .arg("--exact")
        .arg("r0_crash_child_checkpoint")
        .arg("--nocapture")
        .env("ION_R0_TASK_DB", &db)
        .env("ION_R0_TASK_ID", task_id.0.to_string())
        .env("ION_R0_TASK_CHILD", "checkpoint")
        .status()
        .expect("run crash child");
    assert_eq!(status.code(), Some(83));
    assert_eq!(
        fs::read_to_string(&witness)
            .expect("witness")
            .lines()
            .count(),
        1
    );

    let store = new_store(&db);
    let running = task(&store, task_id).expect("running task");
    assert!(matches!(running.status, TaskStatus::Running));
    assert_eq!(
        running.checkpoint,
        Some(serde_json::json!("DurableBeforeCrash"))
    );
    let mut registry = TaskRegistry::default();
    registry.register(CrashCheckpointKind);
    recover_task(&registry, Arc::clone(&store), task_id)
        .await
        .expect("recover");
    let (kind, value) = terminal_value(task(&store, task_id).expect("task").status);
    assert_eq!(
        (kind, value),
        ("completed", serde_json::json!("recovered-after-crash"))
    );
}

#[test]
fn checkpoint_enum_gives_exhaustive_phase_typing_without_a_second_task_framework() {
    fn next(checkpoint: Option<SimpleCheckpoint>) -> &'static str {
        match checkpoint {
            None => "start",
            Some(SimpleCheckpoint::Started) => "recover-or-finish",
        }
    }

    assert_eq!(next(None), "start");
    assert_eq!(next(Some(SimpleCheckpoint::Started)), "recover-or-finish");
    let _ = Completion::<String, String>::Failed("typed failure".to_owned());
}
