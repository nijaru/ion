use std::sync::Arc;

use ion_core::{
    AbortContext, CloseMode, RunningTask, Session, SessionError, TaskCompletion, TaskContext,
    TaskDriver, TaskDriverError, TaskFuture, TaskId, TaskKind, TaskKindName, TaskRegistry,
    TaskRequest, TaskStatus,
};
use serde_json::Value;
use tokio::sync::Notify;

struct Live {
    entered: Arc<Notify>,
    dropped: Arc<Notify>,
    cooperative: bool,
}
struct OnDrop(Arc<Notify>);
impl Drop for OnDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}
impl TaskKind for Live {
    fn execute<'a>(&'a self, _: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let _guard = OnDrop(self.dropped.clone());
            context.checkpoint(Some(Value::Bool(true)), None).await?;
            self.entered.notify_one();
            if self.cooperative {
                context.cancelled().await;
                assert!(context.checkpoint(None, None).await.is_err());
            } else {
                std::future::pending::<()>().await;
            }
            Ok(TaskCompletion::completed(Value::Null))
        })
    }
    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }
    fn abort<'a>(&'a self, _: RunningTask, _: AbortContext) -> TaskFuture<'a> {
        panic!("host close must not call abort")
    }
}
fn setup(cooperative: bool) -> (TaskDriver, TaskId, Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    let mut session = Session::new().unwrap();
    let name = TaskKindName::new("live").unwrap();
    let id = session
        .create_task(TaskRequest {
            conversation_id: session.root_conversation(),
            kind: name.clone(),
            schema_version: 1,
            input: Value::Null,
            dependencies: vec![],
        })
        .unwrap()
        .task_id;
    let mut registry = TaskRegistry::default();
    registry
        .register(
            name,
            1,
            Arc::new(Live {
                entered: entered.clone(),
                dropped: dropped.clone(),
                cooperative,
            }),
        )
        .unwrap();
    (TaskDriver::new(session, registry), id, entered, dropped)
}
async fn close_case(mode: CloseMode) {
    let (driver, id, entered, dropped) = setup(mode == CloseMode::Graceful);
    let caller = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(id).await }
    });
    entered.notified().await;
    let commit = driver.snapshot().await.last_commit;
    driver.close(mode).await;
    dropped.notified().await;
    assert!(matches!(
        caller.await.unwrap(),
        Err(TaskDriverError::Session(SessionError::Closed))
    ));
    let snapshot = driver.snapshot().await;
    assert_eq!(snapshot.last_commit, commit);
    assert!(matches!(snapshot.tasks[0].status, TaskStatus::Running));
    assert!(!snapshot.tasks[0].cancel_requested);
    assert_eq!(snapshot.tasks[0].checkpoint, Some(Value::Bool(true)));
    assert!(driver.drive_task(id).await.is_err());
    assert!(driver.cancel_task(id).await.is_err());
    assert!(driver.wait_task(id).await.is_err());
}
#[tokio::test]
async fn graceful_close_joins_without_durable_cancellation() {
    close_case(CloseMode::Graceful).await;
}
#[tokio::test]
async fn fault_close_drops_and_joins_uncooperative_async_work() {
    close_case(CloseMode::Fault).await;
}
#[tokio::test]
async fn caller_disappearance_does_not_release_live_invocation_ownership() {
    let (driver, id, entered, _) = setup(true);
    let caller = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(id).await }
    });
    entered.notified().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(matches!(
        driver.drive_task(id).await,
        Err(TaskDriverError::AlreadyActive(_))
    ));
    driver.close(CloseMode::Graceful).await;
}
