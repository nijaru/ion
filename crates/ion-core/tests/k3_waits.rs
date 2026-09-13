use std::{
    future::Future,
    num::NonZeroUsize,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use ion_core::{
    AbortContext, ResourceDomain, RunningTask, Session, TaskCapacity, TaskCompletion, TaskContext,
    TaskDriver, TaskFuture, TaskId, TaskKind, TaskKindName, TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::Value;

struct Complete;
impl TaskKind for Complete {
    fn resource_domain(&self) -> Option<ResourceDomain> {
        Some(ResourceDomain::Model)
    }
    fn execute<'a>(&'a self, _: RunningTask, _: TaskContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::completed(Value::Null)) })
    }
    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }
    fn abort<'a>(&'a self, _: RunningTask, _: AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(Value::Null)) })
    }
}
fn task(session: &mut Session, dependencies: Vec<TaskId>) -> TaskId {
    session
        .create_task(TaskRequest {
            conversation_id: session.root_conversation(),
            kind: TaskKindName::new("complete").unwrap(),
            schema_version: 1,
            input: Value::Null,
            dependencies,
        })
        .unwrap()
        .task_id
}
fn pending<F: Future>(future: Pin<&mut F>) {
    assert!(matches!(
        future.poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
}
fn setup() -> (TaskDriver, TaskId, TaskId) {
    let mut session = Session::new().unwrap();
    let first = task(&mut session, vec![]);
    let second = task(&mut session, vec![first]);
    let mut registry = TaskRegistry::default();
    registry
        .register(
            TaskKindName::new("complete").unwrap(),
            1,
            std::sync::Arc::new(Complete),
        )
        .unwrap();
    (
        TaskDriver::with_capacity(
            session,
            registry,
            TaskCapacity::default()
                .with_limit(ResourceDomain::Model, NonZeroUsize::new(1).unwrap()),
        ),
        first,
        second,
    )
}

#[tokio::test]
async fn dependency_drive_waits_without_monopolizing_capacity() {
    let (driver, first, second) = setup();
    let mut blocked = Box::pin(driver.drive_task(second));
    pending(blocked.as_mut());
    assert!(matches!(
        driver.snapshot().await.tasks[1].status,
        TaskStatus::Pending
    ));
    driver.drive_task(first).await.unwrap();
    blocked.await.unwrap();
}

#[tokio::test]
async fn waits_observe_commits_before_and_after_subscription() {
    let (driver, first, second) = setup();
    let mut terminal = Box::pin(driver.wait_task(first));
    let mut dependencies = Box::pin(driver.wait_dependencies(second));
    pending(terminal.as_mut());
    pending(dependencies.as_mut());
    driver.drive_task(first).await.unwrap();
    assert!(matches!(
        terminal.await.unwrap().status,
        TaskStatus::Terminal(_)
    ));
    dependencies.await.unwrap();
    assert!(matches!(
        driver.wait_task(first).await.unwrap().status,
        TaskStatus::Terminal(_)
    ));
}

#[tokio::test]
async fn dropping_wait_does_not_cancel_task_and_cancel_wakes_dependency_wait() {
    let (driver, first, second) = setup();
    let mut wait = Box::pin(driver.wait_task(first));
    pending(wait.as_mut());
    drop(wait);
    assert!(!driver.snapshot().await.tasks[0].cancel_requested);
    let mut dependencies = Box::pin(driver.wait_dependencies(second));
    pending(dependencies.as_mut());
    driver.cancel_task(second).await.unwrap();
    assert!(dependencies.await.unwrap().cancel_requested);
    let outcome = driver.drive_task(second).await.unwrap();
    assert_eq!(outcome.outcome.kind, ion_core::TaskOutcomeKind::Aborted);
    assert_eq!(outcome.invocation_kind, ion_core::InvocationKind::Abort);
    assert_eq!(outcome.generation, 1);
}

#[tokio::test]
async fn close_wakes_dependency_waits_without_reserving_work() {
    let (driver, _, second) = setup();
    let mut wait = Box::pin(driver.wait_dependencies(second));
    pending(wait.as_mut());
    driver.close(ion_core::CloseMode::Graceful).await;
    assert!(wait.await.is_err());
    assert!(matches!(
        driver.snapshot().await.tasks[1].status,
        TaskStatus::Pending
    ));
}

#[test]
fn self_and_forward_dependencies_are_rejected_without_committing() {
    let mut session = Session::new().unwrap();
    let before = session.snapshot();
    // The next allocated identity would be the first task's own identity.
    let own_id = TaskId::new(before.last_commit.local_seq().get() + 1).unwrap();
    for dependency in [own_id, TaskId::new(own_id.local_seq().get() + 1).unwrap()] {
        assert!(
            session
                .create_task(TaskRequest {
                    conversation_id: session.root_conversation(),
                    kind: TaskKindName::new("complete").unwrap(),
                    schema_version: 1,
                    input: Value::Null,
                    dependencies: vec![dependency],
                })
                .is_err()
        );
        assert_eq!(session.snapshot().last_commit, before.last_commit);
    }
}
