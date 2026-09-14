//! K5: the real generation/tool chain on top of `ion-ai`.
//!
//! A submitted input is admitted, bound to a turn root, and answered by the
//! registered built-in kinds: generation freezes and dispatches a model request,
//! a tool task records its own result, the post-tools join makes the
//! continuation runnable, and the continuation generation finishes the turn.
//! The `ScriptedModelService` stands in for a provider; everything else is
//! production code.
//!
//! The recovery tests interrupt a real invocation and observe what the
//! replacement invocation does: it must not repeat an unrecorded external
//! action and must not rebuild a different model request.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ProviderError, ProviderErrorKind,
    ResponseTermination, Role, Script, ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::builtin::{
    Builtins, GenerationKind, PostToolsKind, Tool, ToolCatalog, ToolCatalogError, ToolFuture,
    ToolKind,
};
use ion_core::{
    AdmissionReceipt, ConversationId, DriveOutcome, InputBody, InputDisposition, InputMode,
    InputPlacement, InputRequest, InputSender, InvocationKind, RequestKey, Session, SessionError,
    TaskDriver, TaskDriverError, TaskId, TaskKindName, TaskOutcomeKind, TaskRecord, TaskRegistry,
    TaskRequest, TaskStatus,
};
use serde_json::{Value, json};
use tokio::sync::Notify;

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

/// A second tool claiming the same name, used to prove a rejected registration
/// does not replace the registered one.
struct EchoReplacement;

impl Tool for EchoReplacement {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "echo".to_owned(),
            description: "replacement".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    fn call<'a>(&'a self, _arguments: Value) -> ToolFuture<'a> {
        Box::pin(async move { Ok(json!({"replaced": true})) })
    }
}

/// Never returns, and reports entry so a test can cancel after the call was
/// durably dispatched.
struct Blocking {
    entered: Arc<Notify>,
}

impl Tool for Blocking {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "block".to_owned(),
            description: "never returns".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    fn call<'a>(&'a self, _arguments: Value) -> ToolFuture<'a> {
        let entered = self.entered.clone();
        Box::pin(async move {
            entered.notify_one();
            std::future::pending().await
        })
    }
}

/// Counts real calls and panics on the first one, standing in for an external
/// action whose invocation disappeared after the effect landed. The counter
/// only ever increases inside `call`, so a test can prove the action was not
/// repeated.
struct Flaky {
    calls: Arc<AtomicUsize>,
    panic_once: Arc<AtomicBool>,
    retry_safe: bool,
}

impl Tool for Flaky {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "flaky".to_owned(),
            description: "counts calls and loses its first invocation".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    fn retry_safe(&self) -> bool {
        self.retry_safe
    }

    fn call<'a>(&'a self, _arguments: Value) -> ToolFuture<'a> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let panic = self.panic_once.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            if panic {
                panic!("external call landed before the invocation disappeared");
            }
            Ok(json!({"call": call}))
        })
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

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(text.to_owned())],
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
        model: model_ref(),
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

fn model_ref() -> ModelRef {
    ModelRef {
        provider: "test".to_owned(),
        model: "scripted".to_owned(),
    }
}

/// A driver whose tool kind is deliberately not registered yet. Dispatch then
/// leaves the tool task pending, so a test can drive one invocation at a time
/// and observe its interruption directly.
fn driver_without_tool(
    catalog: Arc<ToolCatalog>,
    scripts: impl IntoIterator<Item = Script>,
) -> (TaskDriver, Arc<ScriptedModelService>) {
    let service = Arc::new(ScriptedModelService::new(scripts));
    let mut registry = TaskRegistry::new();
    registry
        .register(
            kind(ion_core::builtin::GENERATION),
            1,
            Arc::new(GenerationKind::new(model_ref(), service.clone(), catalog)),
        )
        .expect("generation");
    registry
        .register(
            kind(ion_core::builtin::POST_TOOLS),
            1,
            Arc::new(PostToolsKind),
        )
        .expect("post-tools");
    (
        TaskDriver::new(Session::new().expect("session"), registry),
        service,
    )
}

fn submission(target: ConversationId, key: &str, text: &str) -> InputRequest {
    InputRequest {
        target,
        sender: InputSender::User,
        mode: InputMode::Submit,
        request_key: Some(RequestKey::new(key).expect("request key")),
        body: InputBody::Text(text.to_owned()),
    }
}

fn turn_request(target: ConversationId) -> TaskRequest {
    TaskRequest {
        conversation_id: target,
        kind: kind(ion_core::builtin::GENERATION),
        schema_version: 1,
        input: json!({}),
        dependencies: Vec::new(),
    }
}

async fn wait(driver: &TaskDriver, task_id: TaskId) -> TaskRecord {
    tokio::time::timeout(Duration::from_secs(10), driver.wait_task(task_id))
        .await
        .expect("the chain must not stall")
        .expect("wait for a task")
}

fn find(driver_tasks: &[TaskRecord], task_kind: &str) -> Vec<TaskId> {
    driver_tasks
        .iter()
        .filter(|task| task.kind == kind(task_kind))
        .map(|task| task.id)
        .collect()
}

async fn submit_and_drive(
    driver: &TaskDriver,
    root: ConversationId,
    key: &str,
    text: &str,
) -> (AdmissionReceipt, TaskId) {
    let submitted = driver
        .submit_input(submission(root, key, text), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("a submission binds its turn root");
    driver.drive_task(turn).await.expect("drive the turn");
    (submitted, turn)
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

    let (submitted, turn) = {
        let submitted = driver
            .submit_input(
                submission(root, "turn-1", "please echo"),
                turn_request(root),
            )
            .await
            .expect("submit");
        let turn = submitted.task_id.expect("a submission binds its turn root");

        // Admission placed the input, opened the slot and started no work: the
        // turn root is still pending until it is explicitly driven, and the
        // accepted message is already history.
        let snapshot = driver.snapshot().await;
        let input = snapshot
            .inputs
            .iter()
            .find(|input| input.id == submitted.input_id)
            .expect("admitted input");
        assert_eq!(
            input.disposition.placement().map(|placed| placed.turn),
            Some(turn)
        );
        assert_eq!(foreground(&snapshot, root), Some(turn));
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == turn)
                .expect("turn root")
                .status,
            TaskStatus::Pending
        );
        driver.drive_task(turn).await.expect("drive the turn");
        (submitted, turn)
    };

    // Readiness dispatch runs asynchronously, so wait for each step of the chain
    // rather than assuming the whole chain finished when the root settled.
    let tasks = driver.snapshot().await.tasks;
    let tool = *find(&tasks, ion_core::builtin::TOOL)
        .first()
        .expect("tool task");
    let join = *find(&tasks, ion_core::builtin::POST_TOOLS)
        .first()
        .expect("join task");
    wait(&driver, tool).await;
    let join_record = wait(&driver, join).await;
    let TaskStatus::Terminal(join_outcome) = &join_record.status else {
        panic!("the join must be terminal");
    };
    assert_eq!(join_outcome.kind, TaskOutcomeKind::Completed);
    let continuation = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.kind == kind(ion_core::builtin::GENERATION) && task.id != turn)
        .map(|task| task.id)
        .expect("continuation generation");
    wait(&driver, continuation).await;

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
    // The placement, its entry and the answering turn were committed together
    // before the first model request.
    let user_entry = page.entries[0].id;
    let input = snapshot
        .inputs
        .iter()
        .find(|input| input.id == submitted.input_id)
        .expect("admitted input");
    assert_eq!(
        input.disposition.placement(),
        Some(InputPlacement {
            entry: user_entry,
            turn,
        })
    );
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
        foreground(&snapshot, root),
        None,
        "the chain releases the slot"
    );

    // The tool task settled with the tool's own result, not a placeholder.
    let tool_record = snapshot
        .tasks
        .iter()
        .find(|task| task.id == tool)
        .expect("tool task");
    let TaskStatus::Terminal(outcome) = &tool_record.status else {
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
        panic!("the tool task must produce a tool result");
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
    assert_eq!(
        replay.task_id,
        Some(turn),
        "a replay reports the turn that answers the placed input"
    );
    assert_eq!(replay.commit_seq, first.commit_seq);

    let snapshot = driver.snapshot().await;
    assert_eq!(snapshot.inputs.len(), 1);
    assert_eq!(snapshot.tasks.len(), 1, "no second generation was admitted");
    assert_eq!(foreground(&snapshot, root), None);
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
async fn replaying_an_admission_reports_a_queued_input_without_a_turn() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let admitted = session
        .queue_input(submission(root, "turn-1", "hello"))
        .expect("admit without a turn")
        .input_id;
    let driver = TaskDriver::new(session, builtin_registry([] as [Script; 0]));

    // The same key replays the original admission. Because it only queued, the
    // receipt must say so rather than report a turn that does not exist.
    let replayed = driver
        .submit_input(submission(root, "turn-1", "hello"), turn_request(root))
        .await
        .expect("replay");
    assert!(replayed.replayed);
    assert_eq!(replayed.input_id, admitted);
    assert_eq!(replayed.task_id, None, "a queued input has no turn");
    assert!(!replayed.started_turn());
    let snapshot = driver.snapshot().await;
    assert_eq!(snapshot.tasks.len(), 0, "a replay opens no turn");
    assert_eq!(snapshot.inputs[0].disposition, InputDisposition::Queued);
}

/// A failed answer must not strand the accepted request: the placed message is
/// already history, and only the answer is missing.
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

    // The partial answer is not appended, while the accepted input that was
    // placed before dispatch is: a failed answer leaves the request inspectable
    // in the transcript rather than stranded outside it.
    let entries = driver
        .conversation_entries(root, None, 8)
        .await
        .expect("transcript")
        .entries;
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.kind.as_str(), entry.data["text"].clone()))
            .collect::<Vec<_>>(),
        vec![("user", json!("hello"))],
        "only the placed input is history"
    );
    let input = driver
        .snapshot()
        .await
        .inputs
        .into_iter()
        .find(|input| input.id == submitted.input_id)
        .expect("admitted input");
    assert_eq!(
        input.disposition.placement(),
        Some(InputPlacement {
            entry: entries[0].id,
            turn,
        })
    );
}

#[tokio::test]
async fn cancelling_a_turn_closes_the_exchange_and_releases_the_slot() {
    let entered = Arc::new(Notify::new());
    let mut catalog = ToolCatalog::new();
    catalog
        .register(Blocking {
            entered: entered.clone(),
        })
        .expect("register blocking tool");
    let (driver, service) = driver_over(
        Arc::new(catalog),
        [
            Script::Stream(vec![completed(assistant_tool_call(
                "call-1",
                "block",
                json!({}),
            ))]),
            Script::Stream(vec![completed(assistant_text("second turn"))]),
        ],
    );
    let root = driver.snapshot().await.root_conversation;

    let (_, turn) = submit_and_drive(&driver, root, "turn-1", "block please").await;
    let tasks = driver.snapshot().await.tasks;
    let tool = *find(&tasks, ion_core::builtin::TOOL)
        .first()
        .expect("tool task");
    let join = *find(&tasks, ion_core::builtin::POST_TOOLS)
        .first()
        .expect("join task");

    // Cancel only after the call was durably dispatched and entered.
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .expect("the tool call must start");
    driver.cancel_turn(turn).await.expect("cancel the turn");

    // The dispatched call's outcome is unknown and is not reported as stopped,
    // but the call still records a result so the exchange closes.
    let tool_record = wait(&driver, tool).await;
    let TaskStatus::Terminal(outcome) = &tool_record.status else {
        panic!("the cancelled tool must settle");
    };
    assert_eq!(outcome.kind, TaskOutcomeKind::Indeterminate);
    // The never-dispatched join is driven to abort rather than left pending.
    assert_eq!(
        wait(&driver, join).await.status,
        TaskStatus::Terminal(ion_core::TaskOutcome {
            kind: TaskOutcomeKind::Aborted,
            value: json!({"reason": "post-tools join aborted"}),
        })
    );

    let page = driver
        .conversation_entries(root, None, 8)
        .await
        .expect("transcript");
    let kinds: Vec<&str> = page
        .entries
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect();
    assert_eq!(kinds, ["user", "assistant", "tool_result"]);
    assert_eq!(
        foreground(&driver.snapshot().await, root),
        None,
        "cleanup completed, so the slot is free"
    );

    // A later turn builds a provider-safe context from the closed exchange.
    submit_and_drive(&driver, root, "turn-2", "again").await;
    let second: Vec<Role> = service.requests()[1]
        .messages
        .iter()
        .map(|message| message.role)
        .collect();
    assert_eq!(
        second,
        vec![Role::User, Role::Assistant, Role::Tool, Role::User],
        "the aborted call is still represented as a tool result"
    );
    match &service.requests()[1].messages[2].content[0] {
        Content::ToolResult(result) => {
            assert!(
                result.result["error"]
                    .as_str()
                    .is_some_and(|error| { error.contains("outcome is unknown") }),
                "the result must not claim the call was stopped"
            );
        }
        other => panic!("expected a tool result, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_recovery_does_not_repeat_a_dispatched_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut catalog = ToolCatalog::new();
    catalog
        .register(Flaky {
            calls: calls.clone(),
            panic_once: Arc::new(AtomicBool::new(true)),
            retry_safe: false,
        })
        .expect("register flaky tool");
    let catalog = Arc::new(catalog);
    let (driver, service) = driver_without_tool(
        catalog.clone(),
        [
            Script::Stream(vec![completed(assistant_tool_call(
                "call-1",
                "flaky",
                json!({}),
            ))]),
            Script::Stream(vec![completed(assistant_text("done"))]),
        ],
    );
    let root = driver.snapshot().await.root_conversation;

    submit_and_drive(&driver, root, "turn-1", "flaky please").await;
    let tool = *find(&driver.snapshot().await.tasks, ion_core::builtin::TOOL)
        .first()
        .expect("tool task");
    assert_eq!(
        driver.task(tool).await.expect("tool record").status,
        TaskStatus::Pending,
        "an unregistered kind is not dispatched"
    );

    driver
        .register_task_kind(
            kind(ion_core::builtin::TOOL),
            1,
            Arc::new(ToolKind::new(catalog)),
        )
        .expect("register the tool kind");

    // The first invocation lands the external effect and is then lost; the
    // task stays running and recoverable.
    assert!(
        matches!(
            driver.drive_task(tool).await.expect("drive"),
            DriveOutcome::Interrupted(_)
        ),
        "a lost invocation is not a terminal outcome"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        driver.task(tool).await.expect("tool record").status,
        TaskStatus::Running
    );

    // Recovery sees the recorded dispatch and refuses to repeat a call whose
    // external outcome is unknown.
    let DriveOutcome::Settled(settlement) = driver.drive_task(tool).await.expect("recover") else {
        panic!("recovery must settle");
    };
    assert_eq!(settlement.invocation_kind, InvocationKind::Recover);
    assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Indeterminate);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the call must not repeat");

    // The chain still completes, and the continuation sees the uncertainty.
    let join = *find(
        &driver.snapshot().await.tasks,
        ion_core::builtin::POST_TOOLS,
    )
    .first()
    .expect("join task");
    wait(&driver, join).await;
    let continuation = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.kind == kind(ion_core::builtin::GENERATION) && task.id != tool)
        .map(|task| task.id)
        .expect("continuation generation");
    wait(&driver, continuation).await;
    match &service.requests()[1].messages[2].content[0] {
        Content::ToolResult(result) => assert_eq!(
            result.result,
            json!({"error": "the previous attempt was dispatched; its outcome is unknown"})
        ),
        other => panic!("expected a tool result, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_recovery_redispatches_a_retry_safe_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut catalog = ToolCatalog::new();
    catalog
        .register(Flaky {
            calls: calls.clone(),
            panic_once: Arc::new(AtomicBool::new(true)),
            retry_safe: true,
        })
        .expect("register flaky tool");
    let catalog = Arc::new(catalog);
    let (driver, _service) = driver_without_tool(
        catalog.clone(),
        [
            Script::Stream(vec![completed(assistant_tool_call(
                "call-1",
                "flaky",
                json!({}),
            ))]),
            Script::Stream(vec![completed(assistant_text("done"))]),
        ],
    );
    let root = driver.snapshot().await.root_conversation;

    submit_and_drive(&driver, root, "turn-1", "flaky please").await;
    let tool = *find(&driver.snapshot().await.tasks, ion_core::builtin::TOOL)
        .first()
        .expect("tool task");
    driver
        .register_task_kind(
            kind(ion_core::builtin::TOOL),
            1,
            Arc::new(ToolKind::new(catalog)),
        )
        .expect("register the tool kind");
    assert!(matches!(
        driver.drive_task(tool).await.expect("drive"),
        DriveOutcome::Interrupted(_)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let DriveOutcome::Settled(settlement) = driver.drive_task(tool).await.expect("recover") else {
        panic!("recovery must settle");
    };
    assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Completed);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "a retry-safe call may be attempted again"
    );
}

#[tokio::test]
async fn generation_recovery_reuses_the_frozen_request() {
    let (driver, service) = builtin_driver([
        Script::OpenError(ProviderError {
            kind: ProviderErrorKind::Transport,
            message: "provider unreachable".to_owned(),
        }),
        Script::Stream(vec![completed(assistant_text("recovered"))]),
    ]);
    let root = driver.snapshot().await.root_conversation;

    let submitted = driver
        .submit_input(submission(root, "turn-1", "hello"), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("turn root");
    assert!(
        matches!(
            driver.drive_task(turn).await.expect("drive"),
            DriveOutcome::Interrupted(_)
        ),
        "a provider failure interrupts rather than settling"
    );

    // The complete request was frozen before dispatch, with attempt accounting.
    let record = driver.task(turn).await.expect("turn record");
    let checkpoint = record.checkpoint.expect("a frozen request checkpoint");
    assert_eq!(checkpoint["attempts"], json!(1));
    assert_eq!(
        checkpoint["request"]["messages"],
        json!([user_message("hello")])
    );

    let DriveOutcome::Settled(settlement) = driver.drive_task(turn).await.expect("recover") else {
        panic!("recovery must settle");
    };
    assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Completed);
    assert_eq!(settlement.outcome.value["attempts"], json!(2));

    // The replacement invocation replayed the recorded request verbatim.
    let requests = service.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0], requests[1],
        "recovery must not rebuild a different request"
    );

    let page = driver
        .conversation_entries(root, None, 8)
        .await
        .expect("transcript");
    assert_eq!(page.entries[1].data["attempts"], json!(2));
}

#[tokio::test]
async fn a_duplicate_tool_registration_is_rejected_without_replacing() {
    let mut catalog = ToolCatalog::new();
    catalog.register(Echo).expect("first registration");
    let error = catalog
        .register(EchoReplacement)
        .expect_err("a duplicate name must be rejected");
    assert!(matches!(error, ToolCatalogError::Duplicate(name) if name == "echo"));
    let specs = catalog.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(
        specs[0].description, "return the arguments it was called with",
        "the rejected registration must not replace the tool"
    );
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

/// The built-in registry without a driver, for tests that need their own
/// session handle.
fn builtin_registry(scripts: impl IntoIterator<Item = Script>) -> TaskRegistry {
    let mut catalog = ToolCatalog::new();
    catalog.register(Echo).expect("register echo");
    let mut registry = TaskRegistry::new();
    Builtins {
        model: model_ref(),
        service: Arc::new(ScriptedModelService::new(scripts)),
        tools: Arc::new(catalog),
    }
    .register(&mut registry)
    .expect("register built-ins");
    registry
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
