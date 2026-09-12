pub mod r0_support;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use r0_support::{
    AbortPlan, BoxFuture, PrototypeTaskStore, RunningTask, TaskContext, TaskError, TaskKind,
    TaskRegistry, TaskStatus, TerminalPlan, create_task, execute_task, recover_task, shared_store,
    task,
};
use serde::{Deserialize, Serialize};
use tempfile::tempdir;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum RecoveryClass {
    RetrySafe,
    Reconcile,
    NoSafeRetry,
}

impl RecoveryClass {
    fn child_name(self) -> &'static str {
        match self {
            Self::RetrySafe => "retry-safe",
            Self::Reconcile => "reconcile",
            Self::NoSafeRetry => "no-safe-retry",
        }
    }

    fn crash_code(self) -> i32 {
        match self {
            Self::RetrySafe => 91,
            Self::Reconcile => 92,
            Self::NoSafeRetry => 93,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ExternalInput {
    class: RecoveryClass,
    witness: String,
    external_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Attempt {
    number: u32,
    possible_external_effect: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum ExternalCheckpoint {
    Dispatched {
        external_key: String,
        attempts: Vec<Attempt>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum ExternalCompleted {
    Executed { attempts: u32 },
    Adopted { attempts: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum ExternalFailure {
    Indeterminate { attempts: u32 },
}

struct ExternalRecoveryKind;

impl TaskKind for ExternalRecoveryKind {
    const KIND: &'static str = "r0.external-recovery";

    type Input = ExternalInput;
    type Checkpoint = ExternalCheckpoint;
    type Completed = ExternalCompleted;
    type Failure = ExternalFailure;
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
            let checkpoint = ExternalCheckpoint::Dispatched {
                external_key: task.input.external_key.clone(),
                attempts: vec![Attempt {
                    number: 1,
                    possible_external_effect: true,
                }],
            };
            context.commit(|tx| tx.checkpoint(checkpoint))?;
            append_witness(Path::new(&task.input.witness), "attempt-1");

            if std::env::var("ION_R0_RECOVERY_CHILD").ok().as_deref()
                == Some(task.input.class.child_name())
            {
                std::process::exit(task.input.class.crash_code());
            }

            Ok(TerminalPlan::completed(ExternalCompleted::Executed {
                attempts: 1,
            }))
        })
    }

    fn recover<'a>(
        &'a self,
        task: RunningTask<Self::Input, Self::Checkpoint>,
        context: TaskContext<Self::Checkpoint>,
    ) -> BoxFuture<
        'a,
        Result<TerminalPlan<Self::Checkpoint, Self::Completed, Self::Failure>, TaskError>,
    > {
        Box::pin(async move {
            let ExternalCheckpoint::Dispatched {
                external_key,
                mut attempts,
            } = task
                .checkpoint
                .expect("crashed task must retain dispatch checkpoint");
            assert_eq!(external_key, task.input.external_key);
            assert_eq!(attempts.len(), 1);

            match task.input.class {
                RecoveryClass::RetrySafe => {
                    attempts.push(Attempt {
                        number: 2,
                        possible_external_effect: true,
                    });
                    let checkpoint = ExternalCheckpoint::Dispatched {
                        external_key,
                        attempts: attempts.clone(),
                    };
                    context.commit(|tx| tx.checkpoint(checkpoint))?;
                    append_witness(Path::new(&task.input.witness), "attempt-2");
                    Ok(TerminalPlan::completed(ExternalCompleted::Executed {
                        attempts: attempts.len() as u32,
                    }))
                }
                RecoveryClass::Reconcile => {
                    let observed = witness_lines(Path::new(&task.input.witness));
                    assert_eq!(observed, vec!["attempt-1"]);
                    Ok(TerminalPlan::completed(ExternalCompleted::Adopted {
                        attempts: attempts.len() as u32,
                    }))
                }
                RecoveryClass::NoSafeRetry => {
                    Ok(TerminalPlan::failed(ExternalFailure::Indeterminate {
                        attempts: attempts.len() as u32,
                    }))
                }
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

fn append_witness(path: &Path, value: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open external witness");
    writeln!(file, "{value}").expect("append external witness");
    file.sync_all().expect("sync external witness");
}

fn witness_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .expect("read external witness")
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

fn new_store(path: &Path) -> r0_support::SharedStore {
    shared_store(PrototypeTaskStore::open(path).expect("open prototype store"))
}

#[tokio::test]
async fn r0_external_recovery_crash_child() {
    let Some(db) = std::env::var_os("ION_R0_RECOVERY_DB") else {
        return;
    };
    let Some(_mode) = std::env::var_os("ION_R0_RECOVERY_CHILD") else {
        return;
    };
    let task_id = r0_support::TaskId(
        std::env::var("ION_R0_RECOVERY_TASK_ID")
            .expect("task id")
            .parse()
            .expect("numeric task id"),
    );
    let store = new_store(Path::new(&db));
    let mut registry = TaskRegistry::default();
    registry.register(ExternalRecoveryKind);
    execute_task(&registry, store, task_id)
        .await
        .expect("child exits after external action");
}

fn crash_after_dispatch(
    directory: &Path,
    class: RecoveryClass,
) -> (r0_support::SharedStore, r0_support::TaskId, PathBuf) {
    let db = directory.join(format!("{}.sqlite", class.child_name()));
    let witness = directory.join(format!("{}.witness", class.child_name()));
    let store = new_store(&db);
    let task_id = create_task::<ExternalRecoveryKind>(
        &store,
        &ExternalInput {
            class,
            witness: witness.to_string_lossy().into_owned(),
            external_key: format!("key-{}", class.child_name()),
        },
    )
    .expect("create task");
    drop(store);

    let executable = std::env::current_exe().expect("test executable");
    let status = Command::new(executable)
        .arg("--exact")
        .arg("r0_external_recovery_crash_child")
        .arg("--nocapture")
        .env("ION_R0_RECOVERY_DB", &db)
        .env("ION_R0_RECOVERY_TASK_ID", task_id.0.to_string())
        .env("ION_R0_RECOVERY_CHILD", class.child_name())
        .status()
        .expect("run crash child");
    assert_eq!(status.code(), Some(class.crash_code()));

    let store = new_store(&db);
    assert!(matches!(
        task(&store, task_id).expect("crashed task").status,
        TaskStatus::Running
    ));
    assert_eq!(witness_lines(&witness), vec!["attempt-1"]);
    (store, task_id, witness)
}

#[tokio::test]
async fn retry_safe_recovery_records_prior_uncertain_attempt_and_retries() {
    let directory = tempdir().expect("tempdir");
    let (store, task_id, witness) =
        crash_after_dispatch(directory.path(), RecoveryClass::RetrySafe);
    let mut registry = TaskRegistry::default();
    registry.register(ExternalRecoveryKind);

    recover_task(&registry, Arc::clone(&store), task_id)
        .await
        .expect("retry-safe recovery");
    assert_eq!(witness_lines(&witness), vec!["attempt-1", "attempt-2"]);
    let stored = task(&store, task_id).expect("terminal task");
    assert_eq!(
        stored.checkpoint,
        Some(serde_json::json!({
            "Dispatched": {
                "external_key": "key-retry-safe",
                "attempts": [
                    {"number": 1, "possible_external_effect": true},
                    {"number": 2, "possible_external_effect": true}
                ]
            }
        }))
    );
    assert!(matches!(
        stored.status,
        TaskStatus::Terminal { ref kind, .. } if kind == "completed"
    ));
}

#[tokio::test]
async fn reconcile_recovery_adopts_by_durable_external_key_without_repeating() {
    let directory = tempdir().expect("tempdir");
    let (store, task_id, witness) =
        crash_after_dispatch(directory.path(), RecoveryClass::Reconcile);
    let mut registry = TaskRegistry::default();
    registry.register(ExternalRecoveryKind);

    recover_task(&registry, Arc::clone(&store), task_id)
        .await
        .expect("reconcile recovery");
    assert_eq!(witness_lines(&witness), vec!["attempt-1"]);
    assert!(matches!(
        task(&store, task_id).expect("terminal task").status,
        TaskStatus::Terminal { ref kind, .. } if kind == "completed"
    ));
}

#[tokio::test]
async fn no_safe_retry_recovery_becomes_indeterminate_without_repeating() {
    let directory = tempdir().expect("tempdir");
    let (store, task_id, witness) =
        crash_after_dispatch(directory.path(), RecoveryClass::NoSafeRetry);
    let mut registry = TaskRegistry::default();
    registry.register(ExternalRecoveryKind);

    recover_task(&registry, Arc::clone(&store), task_id)
        .await
        .expect("indeterminate recovery");
    assert_eq!(witness_lines(&witness), vec!["attempt-1"]);
    assert!(matches!(
        task(&store, task_id).expect("terminal task").status,
        TaskStatus::Terminal { ref kind, .. } if kind == "failed"
    ));
}
