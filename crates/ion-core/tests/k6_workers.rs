//! K6: a settlement can create owned conversations atomically.
//!
//! A worker is an owned conversation, so creating one is a plan write like any
//! other: the conversation, its reciprocal ownership edge, any entries seeding it
//! and any task running in it become durable in the same commit as the outcome
//! that created them. Nothing here is a separate agent registry or runtime.

use std::sync::Arc;

use ion_core::conversation::context::ContextControl;
use ion_core::{
    CloseMode, ConversationId, ConversationSpec, DriveOutcome, EntryKind, HistoryParent,
    PlannedConversation, PlannedConversationRef, PlannedEntry, PlannedTarget, PlannedTask,
    RunningTask, Session, SessionError, TaskCompletion, TaskContext, TaskDriver, TaskDriverError,
    TaskFuture, TaskKind, TaskKindName, TaskPlan, TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::json;

mod support;

use support::TempDb;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

/// What the spawning kind does: create one worker conversation, seed it with a
/// brief, and start a retained task in it.
enum Seed {
    Fresh,
    Inherited(HistoryParent),
    /// Target a handle minted by a different plan.
    Foreign(PlannedConversationRef),
}

struct SpawnWorker {
    seed: Seed,
}

impl TaskKind for SpawnWorker {
    fn execute<'a>(&'a self, _task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            let worker = match self.seed {
                Seed::Fresh => plan.create_conversation(PlannedConversation::fresh()),
                Seed::Inherited(parent) => {
                    plan.create_conversation(PlannedConversation::inherited(parent))
                }
                Seed::Foreign(reference) => {
                    // Create one first, so a rejection must roll it back too.
                    plan.create_conversation(PlannedConversation::fresh());
                    plan.create_task(PlannedTask {
                        conversation_id: PlannedTarget::planned(reference),
                        kind: kind("worker"),
                        schema_version: 1,
                        input: json!({}),
                        dependencies: Vec::new(),
                        background: true,
                    });
                    return Ok(TaskCompletion::completed(json!("foreign")).with_plan(plan));
                }
            };
            plan.append_entry(PlannedEntry {
                conversation_id: PlannedTarget::planned(worker),
                kind: EntryKind::new("brief").expect("entry kind"),
                data: json!({"text": "do the thing"}),
                projection: Vec::new(),
                context: ContextControl::none(),
            });
            plan.create_task(PlannedTask {
                conversation_id: PlannedTarget::planned(worker),
                kind: kind("worker"),
                schema_version: 1,
                input: json!({}),
                dependencies: Vec::new(),
                background: true,
            });
            Ok(TaskCompletion::completed(json!("spawned")).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

fn registry(seed: Seed) -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    registry
        .register(kind("spawn"), 1, Arc::new(SpawnWorker { seed }))
        .expect("register spawn");
    registry
}

/// Drive one spawning turn and return the driver, the worker it created and the
/// settling task. The worker kind is deliberately unregistered, so the task it
/// starts stays pending and the assertions see exactly what the plan wrote.
async fn drive_spawn(
    mut session: Session,
    seed: Seed,
) -> (TaskDriver, ConversationId, ConversationId) {
    let root = session.root_conversation();
    let turn = session
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: kind("spawn"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .expect("turn");
    let driver = TaskDriver::new(session, registry(seed));
    match driver.drive_task(turn.task_id).await.expect("drive") {
        DriveOutcome::Settled(_) => {}
        other => panic!("expected settlement, got {other:?}"),
    }
    let snapshot = driver.snapshot().await;
    let worker = snapshot
        .conversations
        .iter()
        .find(|conversation| conversation.id != root)
        .map(|conversation| conversation.id)
        .expect("plan-created conversation");
    (driver, worker, root)
}

fn entry_request(conversation_id: ConversationId, text: &str) -> ion_core::EntryRequest {
    ion_core::EntryRequest {
        conversation_id,
        kind: EntryKind::new("user").expect("entry kind"),
        data: json!({"text": text}),
        projection: Vec::new(),
        context: ContextControl::none(),
    }
}

#[tokio::test]
async fn a_plan_created_conversation_is_owned_and_seeded() {
    let session = Session::new().expect("session");
    let (driver, worker, root) = drive_spawn(session, Seed::Fresh).await;
    let snapshot = driver.snapshot().await;

    let conversation = snapshot
        .conversations
        .iter()
        .find(|conversation| conversation.id == worker)
        .expect("worker conversation");
    assert_eq!(conversation.parent, None, "a fresh worker inherits nothing");
    assert_eq!(conversation.foreground_turn, None);
    let settling = conversation.owner_task.expect("owned by its creator");

    let owner = snapshot
        .tasks
        .iter()
        .find(|task| task.id == settling)
        .expect("settling task");
    assert_eq!(owner.conversation_id, root);
    assert_eq!(
        owner.owned_conversations,
        vec![worker],
        "ownership is recorded on both sides in one commit"
    );

    // The brief and the worker's task target the conversation the same plan
    // created, and the retained task does not occupy the root's turn.
    let seeded: Vec<_> = snapshot
        .entries
        .iter()
        .filter(|entry| entry.conversation_id == worker)
        .collect();
    assert_eq!(seeded.len(), 1);
    assert_eq!(seeded[0].kind.as_str(), "brief");
    let worker_task = snapshot
        .tasks
        .iter()
        .find(|task| task.conversation_id == worker)
        .expect("worker task");
    assert_eq!(worker_task.turn, None, "a retained worker is background");
    assert_eq!(worker_task.status, TaskStatus::Pending);
}

#[tokio::test]
async fn an_inherited_worker_starts_from_a_stable_cutoff() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let first = session
        .append_entry(entry_request(root, "first"))
        .expect("entry")
        .entry_id;
    session
        .append_entry(entry_request(root, "second"))
        .expect("entry");

    // Inherit at the first entry: a stable cutoff, not "everything so far".
    let seed = Seed::Inherited(HistoryParent {
        conversation_id: root,
        at: first,
    });
    let (driver, worker, _root) = drive_spawn(session, seed).await;

    let visible = driver
        .conversation_entries(worker, None, 16)
        .await
        .expect("worker transcript");
    let kinds: Vec<(&str, &str)> = visible
        .entries
        .iter()
        .map(|entry| {
            (
                entry.kind.as_str(),
                entry.data["text"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        vec![("user", "first"), ("brief", "do the thing")],
        "an inherited worker sees the cutoff prefix and its own seed"
    );
    // The parent keeps its own history.
    assert_eq!(
        driver
            .conversation_entries(root, None, 16)
            .await
            .expect("root transcript")
            .entries
            .len(),
        2
    );
}

#[tokio::test]
async fn a_foreign_conversation_handle_is_refused_without_partial_settlement() {
    // A handle minted by one plan must not resolve inside another.
    let foreign = TaskPlan::new().create_conversation(PlannedConversation::fresh());
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let turn = session
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: kind("spawn"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .expect("turn");
    let driver = TaskDriver::new(session, registry(Seed::Foreign(foreign)));
    let before = driver.snapshot().await;

    let error = driver
        .drive_task(turn.task_id)
        .await
        .expect_err("a foreign handle must be refused");
    assert!(matches!(
        error,
        TaskDriverError::Session(SessionError::Invariant(_))
    ));

    // The whole plan rolled back, including the conversation it created and the
    // settlement itself.
    let after = driver.snapshot().await;
    assert_eq!(after.conversations.len(), before.conversations.len());
    assert_eq!(after.entries.len(), before.entries.len());
    assert_eq!(after.tasks.len(), before.tasks.len());
    assert!(matches!(
        after
            .tasks
            .iter()
            .find(|task| task.id == turn.task_id)
            .expect("turn root")
            .status,
        TaskStatus::Running
    ));
}

#[tokio::test]
async fn an_invisible_inherited_cutoff_is_refused() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let other = session
        .create_conversation(ConversationSpec::independent())
        .expect("conversation")
        .conversation_id;
    // An entry in a different conversation is not a visible cutoff for the root.
    let foreign_entry = session
        .append_entry(entry_request(other, "elsewhere"))
        .expect("entry")
        .entry_id;
    let seed = Seed::Inherited(HistoryParent {
        conversation_id: root,
        at: foreign_entry,
    });

    let turn = session
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: kind("spawn"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .expect("turn");
    let driver = TaskDriver::new(session, registry(seed));
    let error = driver
        .drive_task(turn.task_id)
        .await
        .expect_err("an invisible cutoff must be refused");
    assert!(
        matches!(
            error,
            TaskDriverError::Session(SessionError::InvalidFork(_))
        ),
        "got {error:?}"
    );
    let snapshot = driver.snapshot().await;
    assert_eq!(snapshot.conversations.len(), 2, "no worker was created");
    assert!(matches!(
        snapshot
            .tasks
            .iter()
            .find(|task| task.id == turn.task_id)
            .expect("turn root")
            .status,
        TaskStatus::Running
    ));
}

/// Returns a plan larger than the enforced conversation bound.
struct Oversized;

impl TaskKind for Oversized {
    fn execute<'a>(&'a self, _task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            for _ in 0..=ion_core::MAX_PLAN_CONVERSATIONS {
                plan.create_conversation(PlannedConversation::fresh());
            }
            Ok(TaskCompletion::completed(json!("oversized")).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

#[tokio::test]
async fn the_plan_conversation_bound_is_enforced() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let mut registry = TaskRegistry::new();
    registry
        .register(kind("oversized"), 1, Arc::new(Oversized))
        .expect("register");
    let turn = session
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: kind("oversized"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .expect("turn");
    let driver = TaskDriver::new(session, registry);
    let before = driver.snapshot().await;

    let error = driver
        .drive_task(turn.task_id)
        .await
        .expect_err("an oversized plan must be refused");
    assert!(matches!(
        error,
        TaskDriverError::Session(SessionError::PlanTooLarge { .. })
    ));
    // Nothing was created, and the rejected settlement left the task recoverable
    // rather than terminal.
    let after = driver.snapshot().await;
    assert_eq!(after.conversations.len(), before.conversations.len());
    assert_eq!(after.entries.len(), before.entries.len());
    assert!(matches!(
        after
            .tasks
            .iter()
            .find(|task| task.id == turn.task_id)
            .expect("turn root")
            .status,
        TaskStatus::Running
    ));
}

#[tokio::test]
async fn a_worker_conversation_and_its_task_survive_reopen() {
    let db = TempDb::new("k6-workers");
    let mut session = Session::create(db.path()).expect("create");
    let root = session.root_conversation();
    let turn = session
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: kind("spawn"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .expect("turn");
    let driver = TaskDriver::new(session, registry(Seed::Fresh));
    assert!(matches!(
        driver.drive_task(turn.task_id).await.expect("drive"),
        DriveOutcome::Settled(_)
    ));
    let before = driver.snapshot().await;
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    let reopened = TaskDriver::open(db.path(), registry(Seed::Fresh)).expect("open");
    assert_eq!(reopened.snapshot().await, before);
    let worker = before
        .conversations
        .iter()
        .find(|conversation| conversation.id != root)
        .expect("worker");
    assert_eq!(worker.owner_task, Some(turn.task_id));
    assert_eq!(
        reopened
            .conversation_entries(worker.id, None, 8)
            .await
            .expect("worker transcript")
            .entries
            .len(),
        1
    );
    // Opening a session starts no work: the worker's task is still pending.
    assert_eq!(
        reopened
            .snapshot()
            .await
            .tasks
            .iter()
            .filter(|task| matches!(task.status, TaskStatus::Pending))
            .count(),
        1
    );
}
