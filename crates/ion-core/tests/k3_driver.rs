use std::sync::Arc;

use ion_core::{
    AbortContext, InvocationKind, Session, TaskCompletion, TaskContext, TaskDriver,
    TaskDriverError, TaskFuture, TaskKind, TaskKindName, TaskOutcomeKind, TaskRegistry,
    TaskRequest, TaskRunError, TaskStatus,
};
use serde_json::json;
use tokio::sync::Notify;

struct CheckpointKind;

impl TaskKind for CheckpointKind {
    fn execute<'a>(&'a self, _task: ion_core::RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            context
                .checkpoint(Some(json!({"phase": "execute"})), None)
                .await?;
            Ok(TaskCompletion::completed(json!("executed")))
        })
    }

    fn recover<'a>(&'a self, _task: ion_core::RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            context
                .checkpoint(Some(json!({"phase": "recover"})), None)
                .await?;
            Ok(TaskCompletion::completed(json!("recovered")))
        })
    }

    fn abort<'a>(&'a self, _task: ion_core::RunningTask, context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async move {
            context
                .checkpoint(Some(json!({"phase": "abort"})), None)
                .await?;
            Ok(TaskCompletion::aborted(json!("aborted")))
        })
    }
}

struct BlockingKind {
    started: Arc<Notify>,
}

impl TaskKind for BlockingKind {
    fn execute<'a>(&'a self, _task: ion_core::RunningTask, context: TaskContext) -> TaskFuture<'a> {
        let started = self.started.clone();
        Box::pin(async move {
            started.notify_one();
            context.cancelled().await;
            Ok(TaskCompletion::completed(json!("late success")))
        })
    }

    fn recover<'a>(&'a self, task: ion_core::RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: ion_core::RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async move { Ok(TaskCompletion::aborted(json!("cancelled"))) })
    }
}

struct ErrorKind;

impl TaskKind for ErrorKind {
    fn execute<'a>(&'a self, _task: ion_core::RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move { Err(TaskRunError::new("expected failure")) })
    }

    fn recover<'a>(&'a self, task: ion_core::RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: ion_core::RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async move { Ok(TaskCompletion::aborted(json!("cancelled"))) })
    }
}

struct PanicKind;

impl TaskKind for PanicKind {
    fn execute<'a>(&'a self, _task: ion_core::RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move { panic!("expected task panic") })
    }

    fn recover<'a>(&'a self, task: ion_core::RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: ion_core::RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async move { Ok(TaskCompletion::aborted(json!("cancelled"))) })
    }
}

fn task(session: &mut Session, kind: &str) -> ion_core::TaskReceipt {
    let root = session.root_conversation();
    session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: TaskKindName::new(kind).expect("task kind"),
            schema_version: 1,
            input: json!({"work": true}),
            dependencies: Vec::new(),
        })
        .expect("task")
}

fn driver_with_kind(kind: &str, implementation: Arc<dyn TaskKind>) -> (TaskDriver, ion_core::TaskId) {
    let mut session = Session::new().expect("session");
    let task = task(&mut session, kind);
    let mut registry = TaskRegistry::new();
    registry
        .register(
            TaskKindName::new(kind).expect("kind"),
            1,
            implementation,
        )
        .expect("register");
    (TaskDriver::new(session, registry), task.task_id)
}

#[tokio::test]
async fn driver_executes_checkpointing_task_outside_session_mutation() {
    let (driver, task_id) = driver_with_kind("checkpoint", Arc::new(CheckpointKind));

    let outcome = driver.drive_task(task_id).await.expect("drive task");
    assert_eq!(outcome.invocation_kind, InvocationKind::Execute);
    assert_eq!(outcome.outcome.kind, TaskOutcomeKind::Completed);
    assert!(outcome.settlement_commit > outcome.reservation_commit);

    let snapshot = driver.snapshot().await;
    let record = snapshot
        .tasks
        .iter()
        .find(|record| record.id == task_id)
        .expect("task record");
    assert_eq!(record.checkpoint, Some(json!({"phase": "execute"})));
    assert!(matches!(record.status, TaskStatus::Terminal(_)));
}

#[tokio::test]
async fn cancellation_signals_live_task_then_runs_fresh_abort_invocation() {
    let started = Arc::new(Notify::new());
    let (driver, task_id) = driver_with_kind(
        "blocking",
        Arc::new(BlockingKind {
            started: started.clone(),
        }),
    );

    let drive = {
        let driver = driver.clone();
        tokio::spawn(async move { driver.drive_task(task_id).await })
    };
    started.notified().await;
    let cancellation = driver.cancel_task(task_id).await.expect("cancel task");
    assert!(cancellation.changed);

    let outcome = drive.await.expect("join driver").expect("drive outcome");
    assert_eq!(outcome.invocation_kind, InvocationKind::Abort);
    assert_eq!(outcome.generation, 2);
    assert_eq!(outcome.outcome.kind, TaskOutcomeKind::Aborted);
}

#[tokio::test]
async fn missing_task_kind_settles_unsupported_without_data_loss() {
    let mut session = Session::new().expect("session");
    let task = task(&mut session, "missing");
    let driver = TaskDriver::new(session, TaskRegistry::new());

    let outcome = driver.drive_task(task.task_id).await.expect("drive task");
    assert_eq!(outcome.outcome.kind, TaskOutcomeKind::Unsupported);
    let snapshot = driver.snapshot().await;
    assert_eq!(snapshot.tasks.len(), 1);
    assert!(matches!(snapshot.tasks[0].status, TaskStatus::Terminal(_)));
}

#[tokio::test]
async fn task_error_settles_failed() {
    let (driver, task_id) = driver_with_kind("error", Arc::new(ErrorKind));

    let outcome = driver.drive_task(task_id).await.expect("drive task");
    assert_eq!(outcome.outcome.kind, TaskOutcomeKind::Failed);
    assert_eq!(outcome.outcome.value, json!({"error": "expected failure"}));
}

#[tokio::test]
async fn task_panic_settles_failed() {
    let (driver, task_id) = driver_with_kind("panic", Arc::new(PanicKind));

    let outcome = driver.drive_task(task_id).await.expect("drive task");
    assert_eq!(outcome.outcome.kind, TaskOutcomeKind::Failed);
    assert_eq!(outcome.outcome.value, json!({"error": "task panicked"}));
}

#[tokio::test]
async fn snapshot_does_not_implicitly_drive_pending_work() {
    let (driver, task_id) = driver_with_kind("checkpoint", Arc::new(CheckpointKind));

    let first = driver.snapshot().await;
    let second = driver.snapshot().await;
    let task = first
        .tasks
        .iter()
        .find(|record| record.id == task_id)
        .expect("task record");
    assert!(matches!(task.status, TaskStatus::Pending));
    assert_eq!(task.checkpoint, None);
    assert_eq!(first, second);
}

#[tokio::test]
async fn second_local_drive_is_rejected_while_first_invocation_is_live() {
    let started = Arc::new(Notify::new());
    let (driver, task_id) = driver_with_kind(
        "blocking",
        Arc::new(BlockingKind {
            started: started.clone(),
        }),
    );

    let drive = {
        let driver = driver.clone();
        tokio::spawn(async move { driver.drive_task(task_id).await })
    };
    started.notified().await;
    let duplicate = driver
        .drive_task(task_id)
        .await
        .expect_err("duplicate local drive must fail");
    assert!(matches!(duplicate, TaskDriverError::AlreadyActive(_)));
    driver.cancel_task(task_id).await.expect("cancel task");
    drive.await.expect("join driver").expect("drive outcome");
}
