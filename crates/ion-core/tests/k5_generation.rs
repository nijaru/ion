//! K5: the real generation/tool chain on top of `ion-ai`.
//!
//! A submitted input is admitted, bound to a turn root, and answered by the
//! registered built-in kinds: generation calls the model service, a tool task
//! runs one call, the post-tools join appends the exchange, and the continuation
//! generation finishes the turn. The `ScriptedModelService` stands in for a
//! provider; everything else is production code.

use std::sync::Arc;

use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::builtin::{Builtins, Tool, ToolCatalog, ToolFuture};
use ion_core::{
    DriveOutcome, InputBody, InputDisposition, InputMode, InputRequest, InputSender, RequestKey,
    Session, SessionError, TaskDriver, TaskDriverError, TaskKindName, TaskOutcomeKind,
    TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::{Value, json};

struct Echo;

impl Tool for Echo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "echo".to_owned(),
            description: "return the arguments it was called with".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    fn call<'a>(&'a self, arguments: Value) -> ToolFuture<'a> {
        Box::pin(async move { Ok(json!({"echoed": arguments})) })
    }
}

/// A tool that never returns on its own, so a test can observe that an
/// in-flight call is stopped by the durable cancellation mark plus local signal.
struct Blocking;

impl Tool for Blocking {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "block".to_owned(),
            description: "never returns".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    fn call<'a>(&'a self, _arguments: Value) -> ToolFuture<'a> {
        Box::pin(std::future::pending())
    }
}

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

fn completed(message: Message) -> ModelStreamEvent {
    ModelStreamEvent::Completed(ModelResponse {
        message,
        usage: Usage::known(10, 4),
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

fn assistant_tool_call(id: &str, name: &str, arguments: Value) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments,
        })],
        provider_replay: None,
    }
}

/// A driver with the built-in kinds registered over a scripted model service.
/// The service is returned so a test can inspect the requests the chain made.
fn builtin_driver(
    scripts: impl IntoIterator<Item = Script>,
) -> (TaskDriver, Arc<ScriptedModelService>) {
    let mut catalog = ToolCatalog::new();
    catalog.register(Echo).expect("register echo");
    driver_over(Arc::new(catalog), scripts)
}

fn driver_over(
    catalog: Arc<ToolCatalog>,
    scripts: impl IntoIterator<Item = Script>,
) -> (TaskDriver, Arc<ScriptedModelService>) {
    let service = Arc::new(ScriptedModelService::new(scripts));
    let mut registry = TaskRegistry::new();
    Builtins {
        model: ModelRef {
            provider: "test".to_owned(),
            model: "scripted".to_owned(),
        },
        service: service.clone(),
        tools: catalog,
    }
    .register(&mut registry)
    .expect("register built-ins");
    (
        TaskDriver::new(Session::new().expect("session"), registry),
        service,
    )
}

fn submission(target: ion_core::ConversationId, key: &str, text: &str) -> InputRequest {
    InputRequest {
        target,
        sender: InputSender::User,
        mode: InputMode::Submit,
        request_key: Some(RequestKey::new(key).expect("request key")),
        body: InputBody::Text(text.to_owned()),
    }
}

fn turn_request(target: ion_core::ConversationId) -> TaskRequest {
    TaskRequest {
        conversation_id: target,
        kind: kind(ion_core::builtin::GENERATION),
        schema_version: 1,
        input: json!({}),
        dependencies: Vec::new(),
    }
}

#[tokio::test]
async fn submitted_input_drives_a_real_generation_tool_chain() {
    let (driver, service) = builtin_driver([
        Script::Stream(vec![completed(assistant_tool_call(
            "call-1",
            "echo",
            json!({"value": "hi"}),
        ))]),
        Script::Stream(vec![
            ModelStreamEvent::TextDelta("all ".to_owned()),
            completed(assistant_text("all done")),
        ]),
    ]);
    let root = driver.snapshot().await.root_conversation;

    let submitted = driver
        .submit_input(
            submission(root, "turn-1", "please echo"),
            turn_request(root),
        )
        .await
        .expect("submit");
    assert!(!submitted.replayed);
    let turn = submitted.task_id.expect("a submission binds its turn root");

    // Admission bound the input and opened the slot, but started no work: the
    // turn root is still pending until it is explicitly driven.
    let snapshot = driver.snapshot().await;
    let input = snapshot
        .inputs
        .iter()
        .find(|input| input.id == submitted.input_id)
        .expect("admitted input");
    assert_eq!(input.disposition, InputDisposition::Assigned(turn));
    assert_eq!(
        snapshot
            .conversations
            .iter()
            .find(|conversation| conversation.id == root)
            .expect("conversation")
            .foreground_turn,
        Some(turn)
    );
    assert_eq!(
        snapshot
            .tasks
            .iter()
            .find(|task| task.id == turn)
            .expect("turn root")
            .status,
        TaskStatus::Pending
    );

    let outcome = driver.drive_task(turn).await.expect("drive the turn");
    assert!(matches!(outcome, DriveOutcome::Settled(_)));

    // The chain appended exactly one entry per step, in transcript order.
    let page = driver
        .conversation_entries(root, None, 32)
        .await
        .expect("transcript");
    let kinds: Vec<&str> = page
        .entries
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect();
    assert_eq!(kinds, ["user", "assistant", "tool_result", "assistant"]);

    let snapshot = driver.snapshot().await;
    // Consuming the input is part of the same commit that appended its entry.
    let user_entry = page.entries[0].id;
    let input = snapshot
        .inputs
        .iter()
        .find(|input| input.id == submitted.input_id)
        .expect("admitted input");
    assert_eq!(input.disposition, InputDisposition::Consumed(user_entry));
    assert!(
        snapshot
            .tasks
            .iter()
            .all(|task| matches!(task.status, TaskStatus::Terminal(_)))
    );
    assert_eq!(
        snapshot.tasks.len(),
        4,
        "generation, tool, join, generation"
    );
    assert_eq!(
        snapshot
            .conversations
            .iter()
            .find(|conversation| conversation.id == root)
            .expect("conversation")
            .foreground_turn,
        None,
        "the finished chain releases the slot"
    );

    // The tool task settled with the tool's own result, not a placeholder.
    let tool = snapshot
        .tasks
        .iter()
        .find(|task| task.kind == kind(ion_core::builtin::TOOL))
        .expect("tool task");
    let TaskStatus::Terminal(outcome) = &tool.status else {
        panic!("tool task must be terminal");
    };
    assert_eq!(outcome.kind, TaskOutcomeKind::Completed);
    assert_eq!(outcome.value["call_id"], json!("call-1"));
    assert_eq!(outcome.value["result"], json!({"echoed": {"value": "hi"}}));

    // The first request carried the submitted text and the tool catalogue; the
    // second saw the whole normalized exchange: user, assistant call, tool
    // result, in call order.
    let requests = service.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages.len(), 1);
    assert_eq!(requests[0].messages[0].role, Role::User);
    assert_eq!(
        requests[0]
            .tools
            .iter()
            .map(|spec| &spec.name)
            .collect::<Vec<_>>(),
        vec!["echo"]
    );

    let second: Vec<Role> = requests[1]
        .messages
        .iter()
        .map(|message| message.role)
        .collect();
    assert_eq!(second, vec![Role::User, Role::Assistant, Role::Tool]);
    let Content::ToolResult(result) = &requests[1].messages[2].content[0] else {
        panic!("the join must produce a tool result");
    };
    assert_eq!(result.call_id, "call-1");
    assert_eq!(result.result, json!({"echoed": {"value": "hi"}}));
}

#[tokio::test]
async fn a_completed_submission_replays_instead_of_opening_a_second_turn() {
    let (driver, service) = builtin_driver([
        Script::Stream(vec![completed(assistant_text("done"))]),
        Script::Stream(vec![completed(assistant_text("again"))]),
    ]);
    let root = driver.snapshot().await.root_conversation;

    let first = driver
        .submit_input(submission(root, "turn-1", "hello"), turn_request(root))
        .await
        .expect("first submit");
    let turn = first.task_id.expect("turn root");
    driver.drive_task(turn).await.expect("drive");

    // The same request key after the turn finished must replay the original
    // input instead of opening a new turn, even though the slot is free.
    let replay = driver
        .submit_input(submission(root, "turn-1", "hello"), turn_request(root))
        .await
        .expect("replay");
    assert!(replay.replayed);
    assert_eq!(replay.input_id, first.input_id);
    assert_eq!(replay.task_id, None, "a consumed input has no live binding");
    assert_eq!(replay.commit_seq, first.commit_seq);

    let snapshot = driver.snapshot().await;
    assert_eq!(snapshot.inputs.len(), 1);
    assert_eq!(snapshot.tasks.len(), 1, "no second generation was admitted");
    assert_eq!(
        snapshot
            .conversations
            .iter()
            .find(|conversation| conversation.id == root)
            .expect("conversation")
            .foreground_turn,
        None
    );
    assert_eq!(service.requests().len(), 1, "the replay called no model");

    // The same key with different content is a conflict, not a new turn.
    let conflict = driver
        .submit_input(submission(root, "turn-1", "different"), turn_request(root))
        .await
        .expect_err("conflicting replay must fail");
    assert!(matches!(
        conflict,
        TaskDriverError::Session(SessionError::IdempotencyConflict(_))
    ));
    assert_eq!(driver.snapshot().await.inputs.len(), 1);
}

#[tokio::test]
async fn an_incomplete_answer_is_not_appended_as_history() {
    let (driver, _service) = builtin_driver([Script::Stream(vec![ModelStreamEvent::TextDelta(
        "partial".to_owned(),
    )])]);
    let root = driver.snapshot().await.root_conversation;

    let submitted = driver
        .submit_input(submission(root, "turn-1", "hello"), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("turn root");
    let DriveOutcome::Settled(settlement) = driver.drive_task(turn).await.expect("drive") else {
        panic!("a terminal answer settles");
    };
    assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Failed);
    assert_eq!(
        settlement.outcome.value["reason"],
        json!("model response ended before a complete answer")
    );

    // Nothing was appended and the input is still bound to the task, so the
    // partial answer cannot be mistaken for a finished turn.
    assert!(
        driver
            .conversation_entries(root, None, 8)
            .await
            .expect("transcript")
            .entries
            .is_empty()
    );
    let input = driver
        .snapshot()
        .await
        .inputs
        .into_iter()
        .find(|input| input.id == submitted.input_id)
        .expect("admitted input");
    assert_eq!(input.disposition, InputDisposition::Assigned(turn));
}

#[tokio::test]
async fn cancelling_a_turn_stops_an_in_flight_tool_call() {
    let mut catalog = ToolCatalog::new();
    catalog.register(Blocking).expect("register blocking tool");
    let (driver, _service) = driver_over(
        Arc::new(catalog),
        [Script::Stream(vec![completed(assistant_tool_call(
            "call-1",
            "block",
            json!({}),
        ))])],
    );
    let root = driver.snapshot().await.root_conversation;

    let submitted = driver
        .submit_input(
            submission(root, "turn-1", "block please"),
            turn_request(root),
        )
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("turn root");
    driver.drive_task(turn).await.expect("drive the turn");

    // The generation settled and dispatched the tool, which is now blocked in
    // its call. Cancelling the turn must stop it rather than let it settle with
    // a fabricated result.
    let tool_id = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.kind == kind(ion_core::builtin::TOOL))
        .map(|task| task.id)
        .expect("dispatched tool task");
    driver.cancel_turn(turn).await.expect("cancel the turn");

    let record = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        driver.wait_task(tool_id),
    )
    .await
    .expect("a cancelled tool call must not hang")
    .expect("wait for the tool task");
    let TaskStatus::Terminal(outcome) = &record.status else {
        panic!("the cancelled tool task must settle through abort");
    };
    assert_eq!(outcome.kind, TaskOutcomeKind::Aborted);

    // The user entry and assistant call are already durable; the pending join is
    // cancelled rather than runnable.
    let page = driver
        .conversation_entries(root, None, 8)
        .await
        .expect("transcript");
    let kinds: Vec<&str> = page
        .entries
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect();
    assert_eq!(kinds, ["user", "assistant"]);
    let join = driver
        .snapshot()
        .await
        .tasks
        .into_iter()
        .find(|task| task.kind == kind(ion_core::builtin::POST_TOOLS))
        .expect("join task");
    assert!(join.cancel_requested);
    assert_eq!(join.status, TaskStatus::Pending);
}

#[tokio::test]
async fn a_submission_that_cannot_open_a_turn_admits_nothing() {
    let (driver, _service) = builtin_driver([] as [Script; 0]);
    let root = driver.snapshot().await.root_conversation;

    driver
        .create_turn(turn_request(root))
        .await
        .expect("first turn");
    let before = driver.snapshot().await;

    // The conversation already has a live turn, so the atomic submission must
    // roll back rather than leave an admitted input with no turn.
    let busy = driver
        .submit_input(submission(root, "turn-2", "second"), turn_request(root))
        .await
        .expect_err("a busy conversation rejects a second turn");
    assert!(matches!(
        busy,
        TaskDriverError::Session(SessionError::ForegroundTurnBusy(_))
    ));
    assert_eq!(driver.snapshot().await, before);
}
