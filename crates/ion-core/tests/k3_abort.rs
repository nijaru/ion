use std::num::NonZeroUsize;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use ion_core::{
    AbortContext, CloseMode, InvocationKind, ResourceDomain, RunningTask, Session, TaskCapacity,
    TaskCompletion, TaskContext, TaskDriver, TaskFuture, TaskId, TaskKind, TaskKindName,
    TaskOutcomeKind, TaskRegistry, TaskRequest,
};
use serde_json::Value;
use tokio::sync::Notify;

#[derive(Default)]
struct Probe {
    executes: AtomicUsize,
    aborts: AtomicUsize,
    entered: Notify,
    admission: Notify,
}
impl TaskKind for Probe {
    fn resource_domain(&self) -> Option<ResourceDomain> {
        self.admission.notify_one();
        Some(ResourceDomain::Model)
    }
    fn execute<'a>(&'a self, _: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            self.executes.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            context.cancelled().await;
            Ok(TaskCompletion::completed(Value::Null))
        })
    }
    fn recover<'a>(&'a self, _: RunningTask, _: TaskContext) -> TaskFuture<'a> {
        panic!("unexpected recover")
    }
    fn abort<'a>(&'a self, _: RunningTask, _: AbortContext) -> TaskFuture<'a> {
        Box::pin(async move {
            self.aborts.fetch_add(1, Ordering::SeqCst);
            Ok(TaskCompletion::aborted(Value::String("cleanup ran".into())))
        })
    }
}
fn create(session: &mut Session, dependencies: Vec<TaskId>) -> TaskId {
    session
        .create_task(TaskRequest {
            conversation_id: session.root_conversation(),
            kind: TaskKindName::new("probe").unwrap(),
            schema_version: 1,
            input: Value::Null,
            dependencies,
        })
        .unwrap()
        .task_id
}
fn driver(session: Session, probe: Arc<Probe>) -> TaskDriver {
    let mut registry = TaskRegistry::default();
    registry
        .register(TaskKindName::new("probe").unwrap(), 1, probe)
        .unwrap();
    TaskDriver::with_capacity(
        session,
        registry,
        TaskCapacity::default().with_limit(ResourceDomain::Model, NonZeroUsize::new(1).unwrap()),
    )
}
fn settled(outcome: ion_core::DriveOutcome) -> ion_core::Settlement {
    match outcome {
        ion_core::DriveOutcome::Settled(settlement) => settlement,
        other => panic!("expected settlement, got {other:?}"),
    }
}

#[tokio::test]
async fn cancellation_before_first_drive_dispatches_exactly_one_abort() {
    let mut session = Session::new().unwrap();
    let task = create(&mut session, vec![]);
    let probe = Arc::new(Probe::default());
    let driver = driver(session, probe.clone());
    driver.cancel_task(task).await.unwrap();
    let result = settled(driver.drive_task(task).await.unwrap());
    assert_eq!(result.invocation_kind, InvocationKind::Abort);
    assert_eq!(result.generation, 1);
    assert_eq!(result.outcome.kind, TaskOutcomeKind::Aborted);
    assert_eq!(probe.executes.load(Ordering::SeqCst), 0);
    assert_eq!(probe.aborts.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn cancellation_of_dependency_blocked_drive_dispatches_abort() {
    let mut session = Session::new().unwrap();
    let first = create(&mut session, vec![]);
    let second = create(&mut session, vec![first]);
    let probe = Arc::new(Probe::default());
    let driver = driver(session, probe.clone());
    let drive = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(second).await }
    });
    tokio::task::yield_now().await;
    driver.cancel_task(second).await.unwrap();
    let result = settled(drive.await.unwrap().unwrap());
    assert_eq!(result.outcome.kind, TaskOutcomeKind::Aborted);
    assert_eq!(result.generation, 1);
    assert_eq!(probe.executes.load(Ordering::SeqCst), 0);
    assert_eq!(probe.aborts.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn cancelled_capacity_waiter_cleans_up_while_model_slot_remains_occupied() {
    let mut session = Session::new().unwrap();
    let first = create(&mut session, vec![]);
    let second = create(&mut session, vec![]);
    let probe = Arc::new(Probe::default());
    let driver = driver(session, probe.clone());
    let first_drive = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(first).await }
    });
    probe.entered.notified().await;
    probe.admission.notified().await;
    let second_drive = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(second).await }
    });
    probe.admission.notified().await;
    driver.cancel_task(second).await.unwrap();
    let result = settled(second_drive.await.unwrap().unwrap());
    assert_eq!(result.outcome.kind, TaskOutcomeKind::Aborted);
    assert_eq!(result.invocation_kind, InvocationKind::Abort);
    assert_eq!(result.generation, 1);
    assert_eq!(probe.executes.load(Ordering::SeqCst), 1);
    assert_eq!(probe.aborts.load(Ordering::SeqCst), 1);
    assert!(!first_drive.is_finished());
    driver.close(CloseMode::Graceful).await;
    assert!(first_drive.await.unwrap().is_err());
}
