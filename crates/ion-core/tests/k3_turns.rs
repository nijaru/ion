use std::sync::Arc;

use tokio::sync::Notify;

use ion_core::{
    AbortContext, ConversationSpec, PlannedTask, RunningTask, Session, SessionError,
    TaskCompletion, TaskContext, TaskDriver, TaskFuture, TaskKind, TaskKindName, TaskOutcomeKind,
    TaskPlan, TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::json;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

fn request(session: &Session, name: &str) -> TaskRequest {
    TaskRequest {
        conversation_id: session.root_conversation(),
        kind: kind(name),
        schema_version: 1,
        input: json!({}),
        dependencies: Vec::new(),
    }
}

/// Settles immediately, creating one turn-scoped tool successor and one
/// background successor (a stand-in for a retained worker).
struct FanOut;

impl TaskKind for FanOut {
    fn execute<'a>(&'a self, task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            plan.create_task(PlannedTask {
                conversation_id: task.conversation_id,
                kind: kind("held"),
                schema_version: 1,
                input: json!({"scope": "turn"}),
                dependencies: Vec::new(),
                background: false,
            });
            plan.create_task(PlannedTask {
                conversation_id: task.conversation_id,
                kind: kind("tool"),
                schema_version: 1,
                input: json!({"scope": "retained"}),
                dependencies: Vec::new(),
                background: true,
            });
            Ok(TaskCompletion::completed(json!("fanned out")).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

struct Plain;

impl TaskKind for Plain {
    fn execute<'a>(&'a self, _task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::completed(json!("ok"))) })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

/// Stays in flight until its turn is cancelled, so a test can observe the
/// conversation while a scoped successor still occupies the slot.
struct Held {
    entered: Arc<Notify>,
}

impl TaskKind for Held {
    fn execute<'a>(&'a self, _task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            self.entered.notify_one();
            // Cancellation is level-triggered, so this cannot miss a signal that
            // already arrived. Returning here lets the driver notice the durable
            // mark and run a fresh abort generation.
            context.cancelled().await;
            Ok(TaskCompletion::completed(json!("held until cancelled")))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

fn registry(entered: Arc<Notify>) -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    registry
        .register(kind("fan"), 1, Arc::new(FanOut))
        .expect("fan");
    registry
        .register(kind("held"), 1, Arc::new(Held { entered }))
        .expect("held");
    registry
        .register(kind("tool"), 1, Arc::new(Plain))
        .expect("tool");
    registry
}

fn foreground_turn(
    session: &Session,
    conversation: ion_core::ConversationId,
) -> Option<ion_core::TaskId> {
    session
        .snapshot()
        .conversations
        .iter()
        .find(|record| record.id == conversation)
        .expect("conversation")
        .foreground_turn
}

#[test]
fn one_live_foreground_turn_per_conversation() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    assert_eq!(foreground_turn(&session, root), None);

    let turn = session
        .create_turn(request(&session, "tool"))
        .expect("turn");
    assert_eq!(foreground_turn(&session, root), Some(turn.task_id));
    let record = &session.snapshot().tasks[0];
    assert_eq!(record.turn, Some(turn.task_id));

    let busy = session
        .create_turn(request(&session, "tool"))
        .expect_err("second live turn must be rejected");
    assert!(matches!(busy, SessionError::ForegroundTurnBusy(id) if id == root));

    // An independent conversation has its own slot.
    let other = session
        .create_conversation(ConversationSpec::independent())
        .expect("conversation");
    let other_request = TaskRequest {
        conversation_id: other.conversation_id,
        kind: kind("tool"),
        schema_version: 1,
        input: json!({}),
        dependencies: Vec::new(),
    };
    session
        .create_turn(other_request)
        .expect("turn on other conversation");
    assert!(foreground_turn(&session, other.conversation_id).is_some());
}

#[tokio::test]
async fn foreground_slot_spans_the_whole_chain_not_just_the_root() {
    let mut session = Session::new().expect("session");
    let root_conversation = session.root_conversation();
    let turn = session.create_turn(request(&session, "fan")).expect("turn");
    let entered = Arc::new(Notify::new());
    let driver = TaskDriver::new(session, registry(entered.clone()));

    driver
        .drive_task(turn.task_id)
        .await
        .expect("drive turn root");

    // The plan committed a scoped successor and a ready background successor.
    // Readiness dispatch starts the scoped one, which stays in flight.
    entered.notified().await;
    let snapshot = driver.snapshot().await;
    assert_eq!(
        foreground_turn_from(&snapshot, root_conversation),
        Some(turn.task_id),
        "the root settled, but its scoped successor still owns the slot"
    );
    let scoped: Vec<_> = snapshot
        .tasks
        .iter()
        .filter(|record| record.turn == Some(turn.task_id))
        .map(|record| record.id)
        .collect();
    assert_eq!(scoped.len(), 2);
    let successor = scoped
        .iter()
        .copied()
        .find(|id| *id != turn.task_id)
        .expect("scoped successor");
    assert!(matches!(
        snapshot
            .tasks
            .iter()
            .find(|record| record.id == successor)
            .expect("scoped record")
            .status,
        TaskStatus::Running
    ));
    // Background work never holds the slot, and reaching terminal does not
    // delay its release either.
    let retained = snapshot
        .tasks
        .iter()
        .find(|record| record.turn.is_none())
        .expect("background successor");
    assert!(matches!(retained.status, TaskStatus::Terminal(_)));
    assert!(!retained.cancel_requested);

    let cancellation = driver.cancel_turn(turn.task_id).await.expect("cancel turn");
    assert!(cancellation.changed());
    assert_eq!(cancellation.cancelled, vec![successor]);
    driver.wait_task(successor).await.expect("abort settles");

    let snapshot = driver.snapshot().await;
    assert_eq!(foreground_turn_from(&snapshot, root_conversation), None);

    // With the chain finished, the conversation can admit a new turn.
    driver
        .create_turn(TaskRequest {
            conversation_id: root_conversation,
            kind: kind("fan"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .await
        .expect("next turn after chain completion");
}

#[tokio::test]
async fn cancelling_a_live_turn_releases_the_slot_after_abort_settlement() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let turn = session
        .create_turn(request(&session, "tool"))
        .expect("turn");
    let driver = TaskDriver::new(session, registry(Arc::new(Notify::new())));

    let cancellation = driver.cancel_turn(turn.task_id).await.expect("cancel turn");
    assert_eq!(cancellation.cancelled, vec![turn.task_id]);
    assert!(foreground_turn_from(&driver.snapshot().await, root).is_some());

    match driver.drive_task(turn.task_id).await.expect("drive abort") {
        ion_core::DriveOutcome::Settled(settlement) => {
            assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Aborted);
        }
        other => panic!("expected settlement, got {other:?}"),
    }
    assert_eq!(foreground_turn_from(&driver.snapshot().await, root), None);
}

fn foreground_turn_from(
    snapshot: &ion_core::SessionSnapshot,
    conversation: ion_core::ConversationId,
) -> Option<ion_core::TaskId> {
    snapshot
        .conversations
        .iter()
        .find(|record| record.id == conversation)
        .expect("conversation")
        .foreground_turn
}

/// Abort cleanup that attempts to create foreground and background successors.
/// Used to verify that a cancelled turn admits no new runnable work while still
/// allowing an explicitly background successor.
struct AbortFanOut {
    started: Arc<tokio::sync::Notify>,
}

impl TaskKind for AbortFanOut {
    fn execute<'a>(&'a self, _task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            self.started.notify_one();
            context.cancelled().await;
            Ok(TaskCompletion::aborted(json!("stopped")))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            plan.create_task(PlannedTask {
                conversation_id: task.conversation_id,
                kind: kind("tool"),
                schema_version: 1,
                input: json!({"scope": "cleanup"}),
                dependencies: Vec::new(),
                background: false,
            });
            plan.create_task(PlannedTask {
                conversation_id: task.conversation_id,
                kind: kind("tool"),
                schema_version: 1,
                input: json!({"scope": "retained"}),
                dependencies: Vec::new(),
                background: true,
            });
            Ok(TaskCompletion::aborted(json!("cleaned up")).with_plan(plan))
        })
    }
}

#[tokio::test]
async fn cancelled_turn_admits_no_runnable_successor_from_abort_cleanup() {
    let started = Arc::new(tokio::sync::Notify::new());
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let turn = session
        .create_turn(request(&session, "abortfan"))
        .expect("turn");
    let mut registry = TaskRegistry::new();
    registry
        .register(
            kind("abortfan"),
            1,
            Arc::new(AbortFanOut {
                started: started.clone(),
            }),
        )
        .expect("abortfan");
    registry
        .register(kind("tool"), 1, Arc::new(Plain))
        .expect("tool");
    let driver = TaskDriver::new(session, registry);

    let drive = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(turn.task_id).await }
    });
    started.notified().await;
    driver.cancel_turn(turn.task_id).await.expect("cancel turn");
    match drive.await.unwrap().expect("drive") {
        ion_core::DriveOutcome::Settled(settlement) => {
            assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Aborted);
        }
        other => panic!("expected settlement, got {other:?}"),
    }

    let snapshot = driver.snapshot().await;
    let cleanup = snapshot
        .tasks
        .iter()
        .find(|record| record.input == json!({"scope": "cleanup"}))
        .expect("cleanup successor");
    assert!(
        cleanup.cancel_requested,
        "a successor inheriting a cancelled turn must be born cancelled"
    );
    assert_eq!(cleanup.turn, Some(turn.task_id));
    let retained = snapshot
        .tasks
        .iter()
        .find(|record| record.input == json!({"scope": "retained"}))
        .expect("retained successor");
    assert!(!retained.cancel_requested);
    assert_eq!(retained.turn, None);

    // The born-cancelled member still holds the chain open until it settles.
    assert_eq!(foreground_turn_from(&snapshot, root), Some(turn.task_id));
    driver
        .drive_task(cleanup.id)
        .await
        .expect("abort cleanup successor");
    assert_eq!(foreground_turn_from(&driver.snapshot().await, root), None);
}
