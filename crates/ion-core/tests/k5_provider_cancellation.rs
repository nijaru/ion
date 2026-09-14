//! Cancellation owns both provider opening and stream collection.
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ion_ai::{
    BoxFuture, Content, Message, ModelRef, ModelRequest, ModelResponse, ModelService, ModelStream,
    ModelStreamEvent, ProviderError, ResponseTermination, Role, Script, ScriptedModelService,
    Usage,
};
use ion_core::builtin::{Builtins, GENERATION, ToolCatalog};
use ion_core::{
    CloseMode, DriveOutcome, InvocationKind, Session, TaskDriver, TaskKindName, TaskOutcomeKind,
    TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::json;
use tokio::sync::Notify;

mod support;

use support::TempDb;

struct DropWitness(Arc<Notify>);
impl Drop for DropWitness {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

struct BlockingProvider {
    during_open: bool,
    entered: Arc<Notify>,
    dropped: Arc<Notify>,
}

impl ModelService for BlockingProvider {
    fn stream<'a>(
        &'a self,
        _request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        Box::pin(async move {
            let witness = DropWitness(self.dropped.clone());
            if self.during_open {
                self.entered.notify_one();
                std::future::pending::<()>().await;
            }
            let entered = self.entered.clone();
            let stream: ModelStream = Box::pin(futures_util::stream::once(async move {
                let _witness = witness;
                entered.notify_one();
                std::future::pending().await
            }));
            Ok(stream)
        })
    }
}

async fn cancelled_attempt(during_open: bool) {
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    let mut registry = TaskRegistry::new();
    Builtins {
        model: ModelRef {
            provider: "test".into(),
            model: "blocked".into(),
        },
        service: Arc::new(BlockingProvider {
            during_open,
            entered: entered.clone(),
            dropped: dropped.clone(),
        }),
        tools: Arc::new(ToolCatalog::new()),
    }
    .register(&mut registry)
    .expect("builtins");
    let driver = TaskDriver::new(Session::new().expect("session"), registry);
    let root = driver.snapshot().await.root_conversation;
    let task = driver
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: TaskKindName::new(GENERATION).expect("kind"),
            schema_version: 1,
            input: json!({}),
            dependencies: vec![],
        })
        .await
        .expect("task")
        .task_id;
    let run = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(task).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("provider entered");
    driver
        .cancel_task(task)
        .await
        .expect("durable cancellation");
    tokio::time::timeout(Duration::from_secs(5), dropped.notified())
        .await
        .expect("cancellation must drop provider ownership");
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("old invocation joins and abort settles")
        .expect("join")
        .expect("drive");
    let snapshot = driver.snapshot().await;
    let record = snapshot
        .tasks
        .iter()
        .find(|record| record.id == task)
        .expect("task");
    assert_eq!(record.generation, 2, "fresh abort invocation");
    let TaskStatus::Terminal(aborted) = &record.status else {
        panic!("the aborted generation settles");
    };
    assert_eq!(aborted.kind, TaskOutcomeKind::Aborted);
    assert_eq!(
        aborted.value["attempts"], 1,
        "abort keeps the prepared attempt on the record"
    );
    assert_eq!(
        record.checkpoint.as_ref().expect("dispatch evidence")["attempts"],
        1
    );
    assert!(
        snapshot.entries.is_empty(),
        "no provisional assistant history"
    );
    assert_eq!(snapshot.tasks.len(), 1, "no tool children");
}

#[tokio::test]
async fn cancellation_drops_the_provider_opening_future() {
    cancelled_attempt(true).await;
}

#[tokio::test]
async fn cancellation_drops_stream_collection() {
    cancelled_attempt(false).await;
}

/// A provider that fails the test by being opened at all.
struct NeverOpen;

impl ModelService for NeverOpen {
    fn stream<'a>(
        &'a self,
        _request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        panic!("a durably cancelled generation must not open the provider")
    }
}

fn driver_with(service: Arc<dyn ModelService>) -> TaskDriver {
    TaskDriver::new(Session::new().expect("session"), registry_with(service))
}

fn registry_with(service: Arc<dyn ModelService>) -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    Builtins {
        model: ModelRef {
            provider: "test".into(),
            model: "scripted".into(),
        },
        service,
        tools: Arc::new(ToolCatalog::new()),
    }
    .register(&mut registry)
    .expect("builtins");
    registry
}

async fn turn(driver: &TaskDriver) -> ion_core::TaskId {
    let root = driver.snapshot().await.root_conversation;
    driver
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: TaskKindName::new(GENERATION).expect("kind"),
            schema_version: 1,
            input: json!({}),
            dependencies: vec![],
        })
        .await
        .expect("turn")
        .task_id
}

#[tokio::test]
async fn cancellation_before_dispatch_never_opens_the_provider() {
    let driver = driver_with(Arc::new(NeverOpen));
    let task = turn(&driver).await;

    // The durable mark commits while the task is still pending, so the drive
    // that follows is abort work: no attempt is prepared and no provider I/O
    // happens.
    driver
        .cancel_task(task)
        .await
        .expect("durable cancellation");
    let driven = driver.drive_task(task).await.expect("abort drive");
    let DriveOutcome::Settled(settlement) = driven else {
        panic!("abort work settles the task");
    };
    assert_eq!(settlement.task_id, task);
    assert_eq!(settlement.invocation_kind, InvocationKind::Abort);
    assert_eq!(
        settlement.outcome.value["attempts"], 0,
        "nothing was prepared, so abort reports no attempt"
    );

    let snapshot = driver.snapshot().await;
    let record = snapshot
        .tasks
        .iter()
        .find(|record| record.id == task)
        .expect("task");
    assert_eq!(record.generation, 1, "abort is the first invocation");
    assert!(
        matches!(&record.status, TaskStatus::Terminal(outcome) if outcome.kind == TaskOutcomeKind::Aborted)
    );
    assert!(
        record.checkpoint.is_none(),
        "a never-dispatched attempt records no dispatch evidence"
    );
    assert!(snapshot.entries.is_empty());
}

/// A checkpoint that exists but cannot be read is not an absence of dispatch
/// during abort either: the request may have reached the provider.
#[tokio::test]
async fn abort_of_an_unreadable_attempt_is_indeterminate() {
    let db = TempDb::new("r9-unreadable-abort");
    let driver = TaskDriver::create(db.path(), registry_with(Arc::new(NeverOpen))).expect("create");
    let task = turn(&driver).await;
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    // A damaged database or an older build presents a checkpoint that exists but
    // cannot be read. It must not be read as "never dispatched".
    damage_checkpoint(db.path(), task, "{\"request\": \"truncated\"}");

    let reopened = TaskDriver::open(db.path(), registry_with(Arc::new(NeverOpen))).expect("reopen");
    reopened
        .cancel_task(task)
        .await
        .expect("durable cancellation");
    let DriveOutcome::Settled(settlement) = reopened.drive_task(task).await.expect("abort drive")
    else {
        panic!("abort must settle rather than retry forever");
    };
    assert_eq!(settlement.invocation_kind, InvocationKind::Abort);
    assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Indeterminate);
    assert_eq!(settlement.outcome.value["attempts"], "unreadable");
    reopened.close(CloseMode::Graceful).await;
}

/// Overwrite a task's stored checkpoint behind a closed session, the way a
/// damaged database or an older build would present it.
fn damage_checkpoint(path: &Path, task: ion_core::TaskId, checkpoint: &str) {
    let connection = rusqlite::Connection::open(path).expect("open raw");
    let updated = connection
        .execute(
            "UPDATE tasks SET checkpoint = ?2 WHERE id = ?1",
            rusqlite::params![task.get(), checkpoint],
        )
        .expect("write the checkpoint");
    assert_eq!(updated, 1, "the task row must exist");
}

/// The other writer order: the answer settles first, so the cancellation that
/// arrives afterwards cannot rewrite a terminal outcome or invent a second
/// attempt.
#[tokio::test]
async fn a_settled_answer_outlives_a_later_cancellation() {
    let service = Arc::new(ScriptedModelService::new([Script::Stream(vec![
        ModelStreamEvent::Completed(ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::Text("answered".to_owned())],
                provider_replay: None,
            },
            usage: Usage::known(7, 2),
            termination: ResponseTermination::Completed,
        }),
    ])]));
    let driver = driver_with(service);
    let task = turn(&driver).await;
    driver.drive_task(task).await.expect("drive the answer");
    let settled = driver
        .snapshot()
        .await
        .tasks
        .into_iter()
        .find(|record| record.id == task)
        .expect("task");
    let TaskStatus::Terminal(settled_outcome) = settled.status.clone() else {
        panic!("the answer settles");
    };
    assert_eq!(settled_outcome.kind, TaskOutcomeKind::Completed);

    let cancellation = driver
        .cancel_turn(task)
        .await
        .expect("cancel a closed turn");
    assert!(
        cancellation.cancelled.is_empty(),
        "no live member to cancel"
    );

    let after = driver.snapshot().await;
    let record = after
        .tasks
        .iter()
        .find(|record| record.id == task)
        .expect("task");
    assert_eq!(record.generation, 1, "a terminal task is not re-driven");
    assert_eq!(record.status, TaskStatus::Terminal(settled_outcome));
    assert_eq!(after.entries.len(), 1, "the answer stays in history");
    assert_eq!(after.entries[0].kind.as_str(), "assistant");
}
