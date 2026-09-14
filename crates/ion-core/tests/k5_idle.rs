//! K5 input admission and idle scheduling.
//!
//! Whether an admitted input starts a turn is one durable decision, and queued
//! input is drained when a settlement releases the conversation's turn slot. The
//! tests use a minimal kind that answers whatever input is bound to it, plus one
//! end-to-end test over the real generation chain.

use std::sync::Arc;
use std::time::Duration;

use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, Usage,
};
use ion_core::builtin::{Builtins, ToolCatalog};
use ion_core::{
    CloseMode, ConversationId, InputBody, InputDisposition, InputMode, InputPlacement,
    InputRequest, InputSender, PlannedTask, PlannedTurn, RequestKey, RunningTask, Session,
    SessionError, TaskCompletion, TaskContext, TaskDriver, TaskDriverError, TaskFuture, TaskId,
    TaskKind, TaskKindName, TaskPlan, TaskRecord, TaskRegistry, TurnTemplate,
};
use serde_json::json;

mod support;

use support::TempDb;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

/// Answers the turn it roots. The writer places accepted input when it binds the
/// input to this turn, so the kind appends nothing: what a test sees in the
/// transcript is placement, not answer output.
struct Answer;

impl TaskKind for Answer {
    fn execute<'a>(&'a self, _task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let inputs = context.placed_inputs().await?;
            Ok(TaskCompletion::completed(json!({"answered": inputs.len()})))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

fn answer_registry() -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    registry
        .register(kind("answer"), 1, Arc::new(Answer))
        .expect("register answer");
    registry
}

fn answer_template() -> TurnTemplate {
    TurnTemplate::new(kind("answer"))
}

fn driver_with_template() -> TaskDriver {
    TaskDriver::new(Session::new().expect("session"), answer_registry())
        .with_turn_template(answer_template())
}

/// A driver with no turn template: admission can still queue, but nothing starts
/// a turn on its own.
fn driver_without_template() -> TaskDriver {
    TaskDriver::new(Session::new().expect("session"), answer_registry())
}

fn input(target: ConversationId, key: &str, mode: InputMode, text: &str) -> InputRequest {
    InputRequest {
        target,
        sender: InputSender::User,
        mode,
        request_key: Some(RequestKey::new(key).expect("request key")),
        body: InputBody::Text(text.to_owned()),
    }
}

async fn wait(driver: &TaskDriver, task_id: TaskId) -> TaskRecord {
    tokio::time::timeout(Duration::from_secs(10), driver.wait_task(task_id))
        .await
        .expect("the turn must not stall")
        .expect("wait for a task")
}

async fn transcript(driver: &TaskDriver, conversation: ConversationId) -> Vec<(String, String)> {
    driver
        .conversation_entries(conversation, None, 16)
        .await
        .expect("transcript")
        .entries
        .into_iter()
        .map(|entry| {
            (
                entry.kind.as_str().to_owned(),
                entry.data["text"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn foreground(
    snapshot: &ion_core::SessionSnapshot,
    conversation: ConversationId,
) -> Option<TaskId> {
    snapshot
        .conversations
        .iter()
        .find(|record| record.id == conversation)
        .expect("conversation")
        .foreground_turn
}

#[tokio::test]
async fn a_queued_follow_up_starts_the_next_turn_when_the_first_finishes() {
    let driver = driver_with_template();
    let root = driver.snapshot().await.root_conversation;

    let first = driver
        .admit_input(input(root, "turn-1", InputMode::Submit, "first"))
        .await
        .expect("first submission");
    assert!(first.started_turn());
    let first_turn = first.task_id.expect("turn root");

    // The conversation is busy, so a follow-up is queued instead of refused.
    let follow_up = driver
        .admit_input(input(root, "turn-2", InputMode::FollowUp, "second"))
        .await
        .expect("follow-up");
    assert!(
        !follow_up.started_turn(),
        "a busy conversation queues a follow-up"
    );
    let snapshot = driver.snapshot().await;
    assert_eq!(snapshot.tasks.len(), 1, "no second turn was opened");
    assert_eq!(snapshot.inputs[1].disposition, InputDisposition::Queued);

    // Finishing the first turn releases the slot, and the queued input starts
    // its own turn without the client asking.
    driver
        .drive_task(first_turn)
        .await
        .expect("drive the first turn");
    let second_turn = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.id != first_turn)
        .map(|task| task.id)
        .expect("the follow-up opened the next turn");
    wait(&driver, second_turn).await;

    assert_eq!(
        transcript(&driver, root).await,
        vec![
            ("user".to_owned(), "first".to_owned()),
            ("user".to_owned(), "second".to_owned()),
        ]
    );
    let snapshot = driver.snapshot().await;
    // Both inputs are placed: the first when its own turn opened, the second
    // when the released slot bound it to its successor turn.
    let entries = driver
        .conversation_entries(root, None, 8)
        .await
        .expect("transcript")
        .entries;
    assert_eq!(
        snapshot.inputs[0].disposition.placement(),
        Some(InputPlacement {
            entry: entries[0].id,
            turn: first_turn,
        })
    );
    assert_eq!(
        snapshot.inputs[1].disposition.placement(),
        Some(InputPlacement {
            entry: entries[1].id,
            turn: second_turn,
        })
    );
    assert_eq!(foreground(&snapshot, root), None);
}

#[tokio::test]
async fn steering_a_busy_conversation_is_deferred_to_the_next_turn_boundary() {
    let driver = driver_with_template();
    let root = driver.snapshot().await.root_conversation;

    let first = driver
        .admit_input(input(root, "turn-1", InputMode::Submit, "first"))
        .await
        .expect("first submission");
    let first_turn = first.task_id.expect("turn root");
    let steer = driver
        .admit_input(input(root, "turn-2", InputMode::Steer, "steer"))
        .await
        .expect("steer");
    assert!(!steer.started_turn(), "a steer waits for a turn boundary");

    driver.drive_task(first_turn).await.expect("drive");

    // Wait for the successor turn rather than assuming the spawned drive ran.
    let second_turn = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.id != first_turn)
        .map(|task| task.id)
        .expect("the steer opened the next turn");
    wait(&driver, second_turn).await;

    // The in-flight turn never saw the steer; it is answered by its own
    // successor turn. Mid-turn injection is not built yet.
    assert_eq!(
        transcript(&driver, root).await,
        vec![
            ("user".to_owned(), "first".to_owned()),
            ("user".to_owned(), "steer".to_owned()),
        ]
    );
    let snapshot = driver.snapshot().await;
    let placed_from = snapshot
        .inputs
        .iter()
        .map(|input| {
            input
                .disposition
                .placement()
                .map(|placed| (input.id, placed.entry))
        })
        .collect::<Vec<_>>();
    let entries = driver
        .conversation_entries(root, None, 8)
        .await
        .expect("transcript");
    assert_eq!(
        placed_from[0],
        Some((first.input_id, entries.entries[0].id))
    );
    assert_eq!(
        placed_from[1],
        Some((steer.input_id, entries.entries[1].id))
    );
}

#[tokio::test]
async fn queue_only_and_notice_never_start_a_turn() {
    let driver = driver_with_template();
    let root = driver.snapshot().await.root_conversation;

    for (key, mode) in [
        ("queue-only", InputMode::QueueOnly),
        ("notice", InputMode::Notice),
    ] {
        let receipt = driver
            .admit_input(input(root, key, mode, "later"))
            .await
            .expect("admission");
        assert!(!receipt.started_turn(), "{mode:?} must not start a turn");
    }
    let snapshot = driver.snapshot().await;
    assert!(snapshot.tasks.is_empty());
    assert!(
        snapshot
            .inputs
            .iter()
            .all(|input| input.disposition == InputDisposition::Queued)
    );

    // Even an explicit scheduling pass leaves them waiting: only a caller that
    // opens a turn can answer them.
    assert_eq!(
        driver.schedule_next_turn(root).await.expect("schedule"),
        None
    );
    assert!(driver.snapshot().await.tasks.is_empty());
}

#[tokio::test]
async fn submit_while_busy_is_rejected_and_admits_nothing() {
    let driver = driver_with_template();
    let root = driver.snapshot().await.root_conversation;
    driver
        .admit_input(input(root, "turn-1", InputMode::Submit, "first"))
        .await
        .expect("first submission");
    let before = driver.snapshot().await;

    let error = driver
        .admit_input(input(root, "turn-2", InputMode::Submit, "second"))
        .await
        .expect_err("a busy conversation rejects a second submit");
    assert!(matches!(
        error,
        TaskDriverError::Session(SessionError::ForegroundTurnBusy(_))
    ));
    assert_eq!(driver.snapshot().await, before, "nothing was admitted");
}

#[tokio::test]
async fn a_turn_starting_mode_without_a_template_is_refused() {
    let driver = driver_without_template();
    let root = driver.snapshot().await.root_conversation;
    let before = driver.snapshot().await;

    let error = driver
        .admit_input(input(root, "turn-1", InputMode::Submit, "hello"))
        .await
        .expect_err("an idle turn-starting admission needs a turn");
    assert!(matches!(
        error,
        TaskDriverError::Session(SessionError::MissingTurnRequest {
            mode: InputMode::Submit
        })
    ));
    assert_eq!(driver.snapshot().await, before, "nothing was admitted");

    // A mode that only queues needs no turn template at all.
    let queued = driver
        .admit_input(input(root, "queue-only", InputMode::QueueOnly, "later"))
        .await
        .expect("queue-only admission");
    assert!(queued.task_id.is_none());
}

#[tokio::test]
async fn queued_input_is_not_scheduled_without_a_template() {
    let driver = driver_without_template();
    let root = driver.snapshot().await.root_conversation;

    // The explicit path starts the first turn (it carries its own turn request).
    let first = driver
        .submit_input(
            input(root, "turn-1", InputMode::Submit, "first"),
            ion_core::TaskRequest {
                conversation_id: root,
                kind: kind("answer"),
                schema_version: 1,
                input: json!({}),
                dependencies: Vec::new(),
            },
        )
        .await
        .expect("explicit turn");
    let first_turn = first.task_id.expect("turn root");
    let follow_up = driver
        .admit_input(input(root, "turn-2", InputMode::FollowUp, "second"))
        .await
        .expect("follow-up");
    assert!(!follow_up.started_turn());

    // Closing the first turn does not schedule the queued input, because no
    // template says what turn to start.
    driver.drive_task(first_turn).await.expect("drive");
    assert_eq!(driver.snapshot().await.tasks.len(), 1);
    assert_eq!(
        driver.snapshot().await.inputs[1].disposition,
        InputDisposition::Queued
    );
    assert_eq!(
        driver.schedule_next_turn(root).await.expect("schedule"),
        None
    );
}

#[tokio::test]
async fn schedule_next_turn_answers_input_queued_before_the_call() {
    // A conversation reopened with durable queued input has no settlement left to
    // trigger scheduling, so a client asks for it explicitly.
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let queued = session
        .queue_input(input(root, "turn-1", InputMode::FollowUp, "resumed"))
        .expect("queued input")
        .input_id;
    let driver = TaskDriver::new(session, answer_registry()).with_turn_template(answer_template());

    assert!(driver.snapshot().await.tasks.is_empty());
    let turn = driver
        .schedule_next_turn(root)
        .await
        .expect("schedule")
        .expect("a queued input starts a turn");
    wait(&driver, turn).await;

    let snapshot = driver.snapshot().await;
    assert_eq!(
        placement_turn(&snapshot, queued),
        Some(turn),
        "scheduling places the queued input with the turn it started"
    );
    assert_eq!(foreground(&snapshot, root), None);
    // Placement moves the input out of the candidate set, so scheduling it again
    // reports nothing to do instead of answering the same input twice.
    assert_eq!(
        driver.schedule_next_turn(root).await.expect("schedule"),
        None
    );
}

#[tokio::test]
async fn a_queued_follow_up_runs_the_generation_chain_again() {
    let service = Arc::new(ScriptedModelService::new([
        Script::Stream(vec![completed(assistant_text("first answer"))]),
        Script::Stream(vec![completed(assistant_text("second answer"))]),
    ]));
    let mut registry = TaskRegistry::new();
    Builtins {
        model: ModelRef {
            provider: "test".to_owned(),
            model: "scripted".to_owned(),
        },
        service: service.clone(),
        tools: Arc::new(ToolCatalog::new()),
    }
    .register(&mut registry)
    .expect("register built-ins");
    let driver = TaskDriver::new(Session::new().expect("session"), registry)
        .with_turn_template(TurnTemplate::new(kind(ion_core::builtin::GENERATION)));
    let root = driver.snapshot().await.root_conversation;

    let first = driver
        .admit_input(input(root, "turn-1", InputMode::Submit, "first"))
        .await
        .expect("first submission");
    let first_turn = first.task_id.expect("turn root");
    let follow_up = driver
        .admit_input(input(root, "turn-2", InputMode::FollowUp, "second"))
        .await
        .expect("follow-up");
    assert!(!follow_up.started_turn());

    driver
        .drive_task(first_turn)
        .await
        .expect("drive the first turn");
    let second_turn = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.id != first_turn)
        .map(|task| task.id)
        .expect("the follow-up opened the next turn");
    wait(&driver, second_turn).await;

    // The follow-up ran as its own generation, and it saw the first exchange.
    let requests = service.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages.len(), 1);
    assert_eq!(requests[0].messages[0].role, Role::User);
    let second: Vec<Role> = requests[1]
        .messages
        .iter()
        .map(|message| message.role)
        .collect();
    assert_eq!(second, vec![Role::User, Role::Assistant, Role::User]);
    let Content::Text(follow_up_text) = &requests[1].messages[2].content[0] else {
        panic!("the follow-up must reach the model as user content");
    };
    assert_eq!(follow_up_text, "second");
}

fn completed(message: Message) -> ModelStreamEvent {
    ModelStreamEvent::Completed(ModelResponse {
        message,
        usage: Usage::known(1, 1),
        termination: ResponseTermination::Completed,
    })
}

fn assistant_text(text: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::Text(text.to_owned())],
        provider_replay: None,
    }
}

/// Blocks until its turn is cancelled, so a test can hold a conversation busy.
struct Held {
    entered: Arc<tokio::sync::Notify>,
}

impl TaskKind for Held {
    fn execute<'a>(&'a self, _task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            self.entered.notify_one();
            context.cancelled().await;
            Ok(TaskCompletion::completed(json!("held until cancelled")))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("stopped"))) })
    }
}

/// Settles immediately, leaving one successor in another conversation that
/// still belongs to this turn.
struct FanTo {
    other: ConversationId,
}

impl TaskKind for FanTo {
    fn execute<'a>(&'a self, task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            plan.create_task(PlannedTask {
                conversation_id: (self.other).into(),
                kind: kind("answer"),
                schema_version: 1,
                input: json!({}),
                dependencies: Vec::new(),
                turn: PlannedTurn::Inherit,
            });
            let _ = task;
            Ok(TaskCompletion::completed(json!("fanned out")).with_plan(plan))
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
async fn a_settlement_that_released_no_slot_starts_no_queued_turn() {
    // The conversation is idle with queued input, and an unrelated background
    // task settles in it. No turn slot was released, so nothing may start.
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let queued = session
        .queue_input(input(root, "turn-1", InputMode::FollowUp, "waiting"))
        .expect("queued input")
        .input_id;
    let background = session
        .create_task(ion_core::TaskRequest {
            conversation_id: root,
            kind: kind("answer"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .expect("background task")
        .task_id;
    let driver = TaskDriver::new(session, answer_registry()).with_turn_template(answer_template());

    driver
        .drive_task(background)
        .await
        .expect("drive the background task");
    let snapshot = driver.snapshot().await;
    assert_eq!(snapshot.tasks.len(), 1, "no turn was started");
    assert_eq!(
        snapshot
            .inputs
            .iter()
            .find(|input| input.id == queued)
            .expect("queued input")
            .disposition,
        InputDisposition::Queued
    );
}

#[tokio::test]
async fn a_cross_conversation_turn_member_releases_the_slot_owner() {
    // A turn member in another conversation can be the last one alive. The
    // conversation that owns the slot is the one that must be scheduled.
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let other = session
        .create_conversation(ion_core::ConversationSpec::independent())
        .expect("second conversation")
        .conversation_id;
    let queued = session
        .queue_input(input(root, "turn-1", InputMode::FollowUp, "resumed"))
        .expect("queued input")
        .input_id;

    let mut registry = answer_registry();
    registry
        .register(kind("fan-to"), 1, Arc::new(FanTo { other }))
        .expect("register fan");
    let driver = TaskDriver::new(session, registry).with_turn_template(answer_template());

    let first = driver
        .submit_input(
            input(root, "turn-2", InputMode::Submit, "start"),
            ion_core::TaskRequest {
                conversation_id: root,
                kind: kind("fan-to"),
                schema_version: 1,
                input: json!({}),
                dependencies: Vec::new(),
            },
        )
        .await
        .expect("explicit turn");
    let first_turn = first.task_id.expect("turn root");
    driver.drive_task(first_turn).await.expect("drive the root");

    // The member in the other conversation settles; that is the settlement which
    // releases the root conversation's slot.
    let member = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.conversation_id == other)
        .map(|task| task.id)
        .expect("cross-conversation member");
    wait(&driver, member).await;

    let successor = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.id != first_turn && task.id != member)
        .map(|task| task.id)
        .expect("the queued input started a turn in the slot owner");
    wait(&driver, successor).await;
    assert_eq!(
        placement_turn(&driver.snapshot().await, queued),
        Some(successor)
    );
}

/// The turn an input is currently placed with, if it has been placed.
fn placement_turn(
    snapshot: &ion_core::SessionSnapshot,
    input: ion_core::InputId,
) -> Option<TaskId> {
    snapshot
        .inputs
        .iter()
        .find(|candidate| candidate.id == input)
        .expect("admitted input")
        .disposition
        .placement()
        .map(|placed| placed.turn)
}

#[tokio::test]
async fn a_clone_shares_the_turn_configuration() {
    let held = Arc::new(tokio::sync::Notify::new());
    let mut registry = answer_registry();
    registry
        .register(
            kind("held"),
            1,
            Arc::new(Held {
                entered: held.clone(),
            }),
        )
        .expect("register held");

    // The handle that starts work is not the handle that was configured.
    let unconfigured = TaskDriver::new(Session::new().expect("session"), registry);
    let configured = unconfigured.clone().with_turn_template(answer_template());

    let root = configured.snapshot().await.root_conversation;
    let first = configured
        .submit_input(
            input(root, "turn-1", InputMode::Submit, "start"),
            ion_core::TaskRequest {
                conversation_id: root,
                kind: kind("held"),
                schema_version: 1,
                input: json!({}),
                dependencies: Vec::new(),
            },
        )
        .await
        .expect("explicit turn");
    let first_turn = first.task_id.expect("turn root");
    let follow_up = unconfigured
        .admit_input(input(root, "turn-2", InputMode::FollowUp, "next"))
        .await
        .expect("follow-up through the other handle");
    assert!(!follow_up.started_turn(), "the conversation is busy");

    let drive = tokio::spawn({
        let unconfigured = unconfigured.clone();
        async move { unconfigured.drive_task(first_turn).await }
    });
    held.notified().await;
    unconfigured.cancel_turn(first_turn).await.expect("cancel");
    drive.await.unwrap().expect("drive");

    // The unconfigured handle still schedules the queued follow-up, because the
    // turn shape is shared configuration rather than per-handle state.
    let successor = unconfigured
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.id != first_turn)
        .map(|task| task.id)
        .expect("the queued follow-up must start a successor turn");
    wait(&unconfigured, successor).await;
}

#[tokio::test]
async fn an_unregistered_turn_kind_leaves_input_queued() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let queued = session
        .queue_input(input(root, "turn-1", InputMode::FollowUp, "waiting"))
        .expect("queued input")
        .input_id;
    // A driver that cannot run the template's kind must not bind input to a turn
    // that would settle Unsupported.
    let driver =
        TaskDriver::new(session, TaskRegistry::new()).with_turn_template(answer_template());

    assert_eq!(
        driver.schedule_next_turn(root).await.expect("schedule"),
        None
    );
    let snapshot = driver.snapshot().await;
    assert!(snapshot.tasks.is_empty(), "no turn was admitted");
    assert_eq!(
        snapshot
            .inputs
            .iter()
            .find(|input| input.id == queued)
            .expect("queued input")
            .disposition,
        InputDisposition::Queued
    );

    // Once the kind is available the input is still answerable.
    driver
        .register_task_kind(kind("answer"), 1, Arc::new(Answer))
        .expect("register the kind");
    let turn = driver
        .schedule_next_turn(root)
        .await
        .expect("schedule")
        .expect("the queued input now starts a turn");
    wait(&driver, turn).await;
    assert_eq!(placement_turn(&driver.snapshot().await, queued), Some(turn));
}

#[tokio::test]
async fn cancelling_a_turn_still_schedules_the_queued_follow_up() {
    let held = Arc::new(tokio::sync::Notify::new());
    let mut registry = answer_registry();
    registry
        .register(
            kind("held"),
            1,
            Arc::new(Held {
                entered: held.clone(),
            }),
        )
        .expect("register held");
    let driver = TaskDriver::new(Session::new().expect("session"), registry)
        .with_turn_template(answer_template());
    let root = driver.snapshot().await.root_conversation;

    let first = driver
        .submit_input(
            input(root, "turn-1", InputMode::Submit, "start"),
            ion_core::TaskRequest {
                conversation_id: root,
                kind: kind("held"),
                schema_version: 1,
                input: json!({}),
                dependencies: Vec::new(),
            },
        )
        .await
        .expect("explicit turn");
    let first_turn = first.task_id.expect("turn root");
    let follow_up = driver
        .admit_input(input(root, "turn-2", InputMode::FollowUp, "next"))
        .await
        .expect("follow-up");
    assert!(!follow_up.started_turn());

    let drive = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(first_turn).await }
    });
    held.notified().await;
    driver.cancel_turn(first_turn).await.expect("cancel");
    drive.await.unwrap().expect("drive");

    // The abort settlement released the slot, so the follow-up admitted during
    // cancellation runs as its own turn.
    let successor = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.id != first_turn)
        .map(|task| task.id)
        .expect("the queued follow-up must start a successor turn");
    wait(&driver, successor).await;
    assert_eq!(
        placement_turn(&driver.snapshot().await, follow_up.input_id),
        Some(successor)
    );
    // The cancelled turn contributed no answer, but the input it accepted is
    // placed: an unanswered request stays history instead of being stranded
    // outside the transcript, and the follow-up turn answers the conversation as
    // it stands.
    assert_eq!(
        transcript(&driver, root).await,
        vec![
            ("user".to_owned(), "start".to_owned()),
            ("user".to_owned(), "next".to_owned()),
        ]
    );
}

#[tokio::test]
async fn a_reopened_session_resumes_queued_input() {
    let db = TempDb::new("k5-idle");
    let mut session = Session::create(db.path()).expect("create");
    let root = session.root_conversation();
    let queued = session
        .queue_input(input(root, "turn-1", InputMode::FollowUp, "resumed"))
        .expect("queued input")
        .input_id;
    let driver = TaskDriver::new(session, answer_registry());
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    let reopened = TaskDriver::open(db.path(), answer_registry())
        .expect("open")
        .with_turn_template(answer_template());
    assert!(reopened.snapshot().await.tasks.is_empty());
    let turn = reopened
        .schedule_next_turn(root)
        .await
        .expect("schedule")
        .expect("a durable queued input starts a turn");
    wait(&reopened, turn).await;
    assert_eq!(
        placement_turn(&reopened.snapshot().await, queued),
        Some(turn)
    );
}
