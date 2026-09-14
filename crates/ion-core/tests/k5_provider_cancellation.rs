//! Cancellation owns both provider opening and stream collection.
use std::sync::Arc;
use std::time::Duration;

use ion_ai::{BoxFuture, ModelRef, ModelRequest, ModelService, ModelStream, ProviderError};
use ion_core::builtin::{Builtins, GENERATION, ToolCatalog};
use ion_core::{
    Session, TaskDriver, TaskKindName, TaskOutcomeKind, TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::json;
use tokio::sync::Notify;

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
    assert!(
        matches!(&record.status, TaskStatus::Terminal(outcome) if outcome.kind == TaskOutcomeKind::Aborted)
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
