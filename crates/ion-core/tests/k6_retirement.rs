//! K6: retiring a worker conversation.
//!
//! Retirement is a read-only archive of an owned, quiescent worker. History,
//! ownership, terminal outcomes and checkpoints are preserved; new work is
//! rejected through every writer path, including settlement plans; queued input
//! that never started is cancelled in the same commit. Nothing here is deletion
//! or a second lifecycle.

use std::sync::Arc;

use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, Usage,
};
use ion_core::builtin::{BRIEF_ENTRY, Builtins, ToolCatalog, WORKER};
use ion_core::conversation::context::ContextControl;
use ion_core::{
    CloseMode, ConversationId, ConversationSpec, DriveOutcome, EntryKind, InputBody,
    InputDisposition, InputMode, InputRequest, InputSender, PlannedEntry, PlannedTarget,
    RunningTask, Session, SessionError, TaskCompletion, TaskContext, TaskDriver, TaskDriverError,
    TaskFuture, TaskId, TaskKind, TaskKindName, TaskPlan, TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::json;

mod support;

use support::TempDb;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

fn assistant_text(text: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::Text(text.to_owned())],
        provider_replay: None,
    }
}

fn completed(message: Message) -> ModelStreamEvent {
    ModelStreamEvent::Completed(ModelResponse {
        message,
        usage: Usage::known(1, 1),
        termination: ResponseTermination::Completed,
    })
}

fn builtins() -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    Builtins {
        model: ModelRef {
            provider: "test".to_owned(),
            model: "scripted".to_owned(),
        },
        service: Arc::new(ScriptedModelService::new([Script::Stream(vec![
            completed(assistant_text("worker answer")),
        ])])),
        tools: Arc::new(ToolCatalog::new()),
    }
    .register(&mut registry)
    .expect("register built-ins");
    registry
}

/// Spawn one worker and wait until it is quiescent.
async fn spawned_worker() -> (TaskDriver, ConversationId, TaskId, TaskId) {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let spawn = session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind(WORKER),
            schema_version: 1,
            input: json!({"brief": "do the thing"}),
            dependencies: Vec::new(),
        })
        .expect("spawn task");
    let driver = TaskDriver::new(session, builtins());
    assert!(matches!(
        driver.drive_task(spawn.task_id).await.expect("spawn"),
        DriveOutcome::Settled(_)
    ));
    let worker = driver
        .owned_conversations(spawn.task_id)
        .await
        .expect("spawn task")
        .pop()
        .expect("one owned worker");
    let generation = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.conversation_id == worker)
        .map(|task| task.id)
        .expect("worker generation");
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        driver.wait_task(generation),
    )
    .await
    .expect("the worker must not stall")
    .expect("wait");
    (driver, worker, spawn.task_id, generation)
}

/// A kind whose settlement appends one entry to a given conversation, so a plan
/// can be pointed at a retired conversation.
struct WriteTo {
    target: ConversationId,
}

impl TaskKind for WriteTo {
    fn execute<'a>(&'a self, _task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            plan.append_entry(PlannedEntry {
                conversation_id: PlannedTarget::existing(self.target),
                kind: EntryKind::new("note").expect("entry kind"),
                data: json!({"text": "written"}),
                projection: Vec::new(),
                context: ContextControl::none(),
            });
            Ok(TaskCompletion::completed(json!("wrote")).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

fn queue_only(target: ConversationId, key: &str) -> InputRequest {
    InputRequest {
        target,
        sender: InputSender::User,
        mode: InputMode::QueueOnly,
        request_key: Some(ion_core::RequestKey::new(key).expect("request key")),
        body: InputBody::Text("later".to_owned()),
    }
}

#[tokio::test]
async fn retiring_a_quiescent_worker_preserves_its_history() {
    let (driver, worker, spawn, generation) = spawned_worker().await;
    let before = driver.snapshot().await;

    driver
        .retire_conversation(worker)
        .await
        .expect("retire a quiescent worker");

    let record = driver.conversation(worker).await.expect("worker record");
    assert!(record.retired);
    assert_eq!(record.owner_task, Some(spawn), "ownership survives");
    // Everything already durable is still readable.
    let page = driver
        .conversation_entries(worker, None, 8)
        .await
        .expect("transcript");
    let kinds: Vec<&str> = page
        .entries
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect();
    assert_eq!(kinds, vec![BRIEF_ENTRY, "assistant"], "history survives");
    let generation_record = driver.task(generation).await.expect("generation");
    assert!(
        generation_record.checkpoint.is_some(),
        "a terminal task keeps its checkpoint as evidence"
    );
    assert_eq!(
        driver.owned_conversations(spawn).await.expect("spawn task"),
        vec![worker]
    );

    // Retiring changes exactly one field.
    let after = driver.snapshot().await;
    assert_eq!(after.entries, before.entries);
    assert_eq!(after.tasks, before.tasks);
    assert_eq!(after.inputs, before.inputs);
    assert_ne!(after.conversations, before.conversations);
}

#[tokio::test]
async fn a_retired_conversation_rejects_new_work() {
    let (driver, worker, _spawn, _generation) = spawned_worker().await;
    driver.retire_conversation(worker).await.expect("retire");
    let before = driver.snapshot().await;

    let retired = driver
        .create_turn(TaskRequest {
            conversation_id: worker,
            kind: kind("worker"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .await
        .expect_err("no new turn");
    assert!(matches!(
        retired,
        TaskDriverError::Session(SessionError::ConversationRetired(id)) if id == worker
    ));
    let queued = driver
        .admit_input(queue_only(worker, "input"))
        .await
        .expect_err("no new input");
    assert!(matches!(
        queued,
        TaskDriverError::Session(SessionError::ConversationRetired(id)) if id == worker
    ));
    assert_eq!(
        driver.schedule_next_turn(worker).await.expect("schedule"),
        None
    );

    // A settlement plan cannot write there either, and the plan's failure rolls
    // back the settlement rather than half-applying it.
    driver
        .register_task_kind(kind("write"), 1, Arc::new(WriteTo { target: worker }))
        .expect("register write");
    let root = before.root_conversation;
    let writer = driver
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: kind("write"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .await
        .expect("writer turn");
    let error = driver
        .drive_task(writer.task_id)
        .await
        .expect_err("a plan may not write into a retired conversation");
    assert!(matches!(
        error,
        TaskDriverError::Session(SessionError::ConversationRetired(id)) if id == worker
    ));
    assert_eq!(
        driver.task(writer.task_id).await.expect("writer").status,
        TaskStatus::Running,
        "the failed settlement stays recoverable"
    );

    // Reads keep working, and inheriting history from the retired ancestor is
    // read-only, so it stays allowed.
    assert!(driver.conversation(worker).await.expect("record").retired);
    let after = driver.snapshot().await;
    assert_eq!(after.entries, before.entries);
    assert_eq!(after.tasks.len(), before.tasks.len() + 1);
}

#[tokio::test]
async fn retirement_requires_quiescence() {
    let (driver, worker, spawn, _generation) = spawned_worker().await;

    // A live task blocks retirement, and the rejection changes nothing.
    let live = driver
        .create_turn(TaskRequest {
            conversation_id: worker,
            kind: kind(WORKER),
            schema_version: 1,
            input: json!({"brief": "second"}),
            dependencies: Vec::new(),
        })
        .await
        .expect("second turn in the worker");
    let before = driver.snapshot().await;
    let busy = driver
        .retire_conversation(worker)
        .await
        .expect_err("a live task blocks retirement");
    assert!(matches!(
        busy,
        TaskDriverError::Session(SessionError::ConversationHasLiveWork(id)) if id == worker
    ));
    assert_eq!(driver.snapshot().await, before, "rejection is a no-op");

    // Cancel and settle the live turn, then retirement succeeds.
    driver
        .cancel_turn(live.task_id)
        .await
        .expect("cancel the second turn");
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        driver.wait_task(live.task_id),
    )
    .await
    .expect("cleanup must settle")
    .expect("wait");
    driver
        .retire_conversation(worker)
        .await
        .expect("quiescent again");
    assert!(driver.conversation(worker).await.expect("record").retired);
    assert_eq!(
        driver.owned_conversations(spawn).await.expect("spawn"),
        vec![worker]
    );
}

#[tokio::test]
async fn retirement_cancels_input_that_never_started() {
    let (driver, worker, _spawn, _generation) = spawned_worker().await;
    let queued = driver
        .admit_input(queue_only(worker, "queued"))
        .await
        .expect("queue input");
    assert!(!queued.started_turn());

    driver.retire_conversation(worker).await.expect("retire");
    let input = driver
        .snapshot()
        .await
        .inputs
        .into_iter()
        .find(|input| input.id == queued.input_id)
        .expect("input");
    assert_eq!(input.disposition, InputDisposition::Cancelled);
    // The body is retained: retirement stops work, it does not forget input.
    assert_eq!(input.body, InputBody::Text("later".to_owned()));
}

#[tokio::test]
async fn only_owned_workers_can_be_retired() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let independent = session
        .create_conversation(ConversationSpec::independent())
        .expect("conversation")
        .conversation_id;

    let root_error = session
        .retire_conversation(root)
        .expect_err("the root conversation is not a worker");
    assert!(matches!(
        root_error,
        SessionError::ConversationNotOwned(id) if id == root
    ));
    let independent_error = session
        .retire_conversation(independent)
        .expect_err("an unowned conversation is not a worker");
    assert!(matches!(
        independent_error,
        SessionError::ConversationNotOwned(id) if id == independent
    ));
}

#[tokio::test]
async fn reactivation_clears_the_flag_and_starts_nothing() {
    let (driver, worker, _spawn, _generation) = spawned_worker().await;
    driver.retire_conversation(worker).await.expect("retire");
    let retired = driver.snapshot().await;

    driver
        .reactivate_conversation(worker)
        .await
        .expect("reactivate");
    let active = driver.snapshot().await;
    assert!(!driver.conversation(worker).await.expect("record").retired);
    assert_eq!(active.tasks, retired.tasks, "reactivation starts no work");
    assert_eq!(active.entries, retired.entries);
    assert_eq!(active.inputs, retired.inputs);
    // Reactivating an active conversation is idempotent.
    let commit = driver.snapshot().await.last_commit;
    driver
        .reactivate_conversation(worker)
        .await
        .expect("idempotent reactivation");
    assert!(driver.snapshot().await.last_commit > commit);

    // Writes are accepted again.
    driver
        .create_turn(TaskRequest {
            conversation_id: worker,
            kind: kind(WORKER),
            schema_version: 1,
            input: json!({"brief": "third"}),
            dependencies: Vec::new(),
        })
        .await
        .expect("a reactivated conversation accepts work");
}

#[tokio::test]
async fn retirement_survives_reopen() {
    let db = TempDb::new("k6-retirement");
    let mut session = Session::create(db.path()).expect("create");
    let root = session.root_conversation();
    let spawn = session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind(WORKER),
            schema_version: 1,
            input: json!({"brief": "do the thing"}),
            dependencies: Vec::new(),
        })
        .expect("spawn task");
    let driver = TaskDriver::new(session, builtins());
    assert!(matches!(
        driver.drive_task(spawn.task_id).await.expect("spawn"),
        DriveOutcome::Settled(_)
    ));
    let worker = driver
        .owned_conversations(spawn.task_id)
        .await
        .expect("spawn task")
        .pop()
        .expect("worker");
    let generation = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.conversation_id == worker)
        .map(|task| task.id)
        .expect("generation");
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        driver.wait_task(generation),
    )
    .await
    .expect("no stall")
    .expect("wait");
    // Queued input is cancelled by the retirement commit, so it must be durable:
    // a resident-only cancellation would resurrect the input on reopen and
    // contradict "reactivation resurrects nothing".
    let queued = driver
        .admit_input(queue_only(worker, "queued"))
        .await
        .expect("queue input");
    driver.retire_conversation(worker).await.expect("retire");
    let before = driver.snapshot().await;
    assert_eq!(
        before
            .inputs
            .iter()
            .find(|input| input.id == queued.input_id)
            .expect("queued input")
            .disposition,
        InputDisposition::Cancelled
    );
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    let reopened = TaskDriver::open(db.path(), builtins()).expect("open");
    assert_eq!(reopened.snapshot().await, before);
    assert!(reopened.conversation(worker).await.expect("record").retired);
    let rejected = reopened
        .create_turn(TaskRequest {
            conversation_id: worker,
            kind: kind("worker"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .await
        .expect_err("still retired after reopen");
    assert!(matches!(
        rejected,
        TaskDriverError::Session(SessionError::ConversationRetired(id)) if id == worker
    ));
    reopened
        .reactivate_conversation(worker)
        .await
        .expect("reactivate");
    assert!(!reopened.conversation(worker).await.expect("record").retired);
}

#[test]
fn an_older_schema_version_is_refused_rather_than_migrated() {
    // A database written before the retired column existed must be refused, not
    // silently reinterpreted: the pre-1.0 policy is archive-or-refuse.
    let db = TempDb::new("k6-schema");
    let session = Session::create(db.path()).expect("create");
    drop(session);
    {
        let connection = rusqlite::Connection::open(db.path()).expect("open raw");
        connection
            .pragma_update(None, "user_version", 1)
            .expect("downgrade the recorded version");
    }
    let error = Session::open(db.path()).expect_err("an old schema is refused");
    assert!(
        matches!(error, SessionError::Persistence(_)),
        "got {error:?}"
    );
}
