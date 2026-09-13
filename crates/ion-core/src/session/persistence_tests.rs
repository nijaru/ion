use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::*;
use crate::session::transaction::MutationBatch;
use crate::store::StoreError;
use crate::{
    AbortContext, CloseMode, RunningTask, TaskCompletion, TaskContext, TaskDriver, TaskFuture,
    TaskKind, TaskKindName, TaskRegistry,
};
use tokio::sync::Notify;

#[derive(Debug)]
struct FaultStore(Arc<AtomicBool>);
impl Persistence for FaultStore {
    fn commit(&mut self, _: &MutationBatch) -> Result<(), StoreError> {
        if self.0.load(Ordering::SeqCst) {
            Err(StoreError("injected failure".into()))
        } else {
            Ok(())
        }
    }
}
fn create(session: &mut Session) -> TaskId {
    session
        .create_task(TaskRequest {
            conversation_id: session.root_conversation(),
            kind: TaskKindName::new("live").unwrap(),
            schema_version: 1,
            input: Value::Null,
            dependencies: vec![],
        })
        .unwrap()
        .task_id
}

/// A commit must not deep-copy unrelated durable records. Records are shared by
/// `Arc`, so an untouched task keeps its allocation across another task's
/// checkpoint, while the touched record is replaced copy-on-write.
#[test]
fn commit_copies_only_touched_task_records() {
    let mut session = Session::new().expect("session");
    let touched = create(&mut session);
    let untouched = create(&mut session);
    let invocation = session
        .reserve_task_invocation(touched, InvocationKind::Execute)
        .expect("reserve");

    let touched_before = session.task_record_ptr(touched).expect("touched record");
    let untouched_before = session
        .task_record_ptr(untouched)
        .expect("untouched record");

    session
        .checkpoint_task(
            touched,
            invocation.generation,
            Some(serde_json::json!({"phase": "copy-on-write"})),
            None,
        )
        .expect("checkpoint");

    assert_eq!(
        session.task_record_ptr(untouched),
        Some(untouched_before),
        "an untouched task must keep its resident allocation"
    );
    assert_ne!(
        session.task_record_ptr(touched),
        Some(touched_before),
        "the touched task is replaced copy-on-write"
    );
    assert_eq!(
        session.task_record(touched).unwrap().checkpoint,
        Some(serde_json::json!({"phase": "copy-on-write"}))
    );
}

#[test]
fn failed_persistence_does_not_install_draft_or_publish_successors() {
    let mut session = Session::new().unwrap();
    let task = create(&mut session);
    let invocation = session
        .reserve_task_invocation(task, InvocationKind::Execute)
        .unwrap();
    let before = session.snapshot();
    session.store = Box::new(FaultStore(Arc::new(AtomicBool::new(true))));
    let outcome = TaskCompletion::completed(Value::Null).outcome;
    assert!(matches!(
        session.settle_task_with(task, invocation.generation, outcome, None, |transaction| {
            transaction.create_conversation(ConversationSpec {
                parent: None,
                owner_task: Some(task),
            })
        }),
        Err(SessionError::Persistence(_))
    ));
    assert_eq!(session.snapshot(), before);
    assert!(
        session
            .observations_after(Some(before.last_commit))
            .events
            .is_empty()
    );
    assert!(session.fault_signal().is_cancelled());
    assert!(matches!(
        session.mark_task_cancellation(task),
        Err(SessionError::Closed)
    ));
}

struct Live {
    entered: Arc<Notify>,
    dropped: Arc<Notify>,
}
struct DropSignal(Arc<Notify>);
impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}
impl TaskKind for Live {
    fn execute<'a>(&'a self, _: RunningTask, _: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let _guard = DropSignal(self.dropped.clone());
            self.entered.notify_one();
            std::future::pending().await
        })
    }
    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }
    fn abort<'a>(&'a self, _: RunningTask, _: AbortContext) -> TaskFuture<'a> {
        panic!("fault is not durable cancellation")
    }
}

#[tokio::test]
async fn store_failure_fences_and_joins_live_invocations() {
    let mut session = Session::new().unwrap();
    let task = create(&mut session);
    let fail = Arc::new(AtomicBool::new(false));
    session.store = Box::new(FaultStore(fail.clone()));
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    let mut registry = TaskRegistry::default();
    registry
        .register(
            TaskKindName::new("live").unwrap(),
            1,
            Arc::new(Live {
                entered: entered.clone(),
                dropped: dropped.clone(),
            }),
        )
        .unwrap();
    let driver = TaskDriver::new(session, registry);
    let running = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(task).await }
    });
    entered.notified().await;
    let before = driver.snapshot().await;
    fail.store(true, Ordering::SeqCst);
    assert!(driver.cancel_task(task).await.is_err());
    dropped.notified().await;
    assert!(running.await.unwrap().is_err());
    assert_eq!(driver.snapshot().await, before);
    driver.close(CloseMode::Fault).await;
}
