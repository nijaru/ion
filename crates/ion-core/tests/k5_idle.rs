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
use ion_core::conversation::context::ContextControl;
use ion_core::{
    ConversationId, EntryKind, InputBody, InputDisposition, InputMode, InputRequest, InputSender,
    PlannedEntry, RequestKey, RunningTask, Session, SessionError, TaskCompletion, TaskContext,
    TaskDriver, TaskDriverError, TaskFuture, TaskId, TaskKind, TaskKindName, TaskPlan, TaskRecord,
    TaskRegistry, TurnTemplate,
};
use serde_json::json;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

/// Answers every input bound to it by appending one user entry per input, so a
/// test can see exactly which input each turn consumed.
struct Answer;

impl TaskKind for Answer {
    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let inputs = context.assigned_inputs().await?;
            let mut plan = TaskPlan::new();
            for input in &inputs {
                let InputBody::Text(text) = &input.body;
                let reference = plan.append_entry(PlannedEntry {
                    conversation_id: task.conversation_id,
                    kind: EntryKind::new("user").expect("entry kind"),
                    data: json!({"text": text}),
                    projection: Vec::new(),
                    context: ContextControl::none(),
                });
                plan.consume_input(input.id, reference);
            }
            Ok(TaskCompletion::completed(json!({"answered": inputs.len()})).with_plan(plan))
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
    assert_eq!(
        snapshot.inputs[0].disposition,
        InputDisposition::Consumed(
            driver
                .conversation_entries(root, None, 1)
                .await
                .expect("first entry")
                .entries[0]
                .id
        )
    );
    assert!(matches!(
        snapshot.inputs[1].disposition,
        InputDisposition::Consumed(_)
    ));
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
    let consumed_from = snapshot
        .inputs
        .iter()
        .map(|input| match input.disposition {
            InputDisposition::Consumed(entry) => Some((input.id, entry)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let entries = driver
        .conversation_entries(root, None, 8)
        .await
        .expect("transcript");
    assert_eq!(
        consumed_from[0],
        Some((first.input_id, entries.entries[0].id))
    );
    assert_eq!(
        consumed_from[1],
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
    assert!(matches!(
        snapshot
            .inputs
            .iter()
            .find(|input| input.id == queued)
            .expect("input")
            .disposition,
        InputDisposition::Consumed(_)
    ));
    assert_eq!(foreground(&snapshot, root), None);
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
