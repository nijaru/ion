//! K5 recovery: what a replacement invocation does with the checkpoint a lost
//! one left behind.
//!
//! The durable checkpoint is the only evidence recovery has about the attempt it
//! is replacing. These tests corrupt, age and change that evidence behind a real
//! close and reopen, and assert the replacement invocation fails closed: it
//! never rebuilds a frozen model request from changed state, never repeats a call
//! whose external outcome is unknown, and never reports one as known.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ion_ai::{
    Content, Message, ModelRef, ModelRequest, ModelStream, ModelStreamEvent, ProviderError,
    ProviderErrorKind, ResponseTermination, Role, Script, ScriptedModelService, ToolCall, ToolSpec,
    Usage,
};
use ion_core::builtin::{GenerationKind, PostToolsKind, Tool, ToolCatalog, ToolFuture, ToolKind};
use ion_core::{
    CloseMode, ConversationId, DriveOutcome, InputBody, InputMode, InputRequest, InputSender,
    RequestKey, TaskDriver, TaskId, TaskKindName, TaskOutcomeKind, TaskRegistry, TaskRequest,
    TaskStatus,
};
use serde_json::{Value, json};

mod support;

use support::TempDb;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

fn model_ref() -> ModelRef {
    ModelRef {
        provider: "test".to_owned(),
        model: "scripted".to_owned(),
    }
}

fn assistant_tool_call(id: &str, name: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: json!({}),
        })],
        provider_replay: None,
    }
}

fn completed(message: Message) -> ModelStreamEvent {
    ModelStreamEvent::Completed(ion_ai::ModelResponse {
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

/// Counts calls and loses its first invocation after the effect landed, standing
/// in for an external action whose outcome nobody observed. A repeat is visible
/// in the counter.
struct Lost {
    calls: Arc<AtomicUsize>,
    lost: Arc<AtomicBool>,
    retry_safe: bool,
}

impl Tool for Lost {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "lost".to_owned(),
            description: "counts calls and loses its first invocation".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    fn retry_safe(&self) -> bool {
        self.retry_safe
    }

    fn call<'a>(&'a self, _arguments: Value) -> ToolFuture<'a> {
        let count = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let lose = self.lost.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            if lose {
                panic!("external call landed before the invocation disappeared");
            }
            Ok(json!({"call": count}))
        })
    }
}

/// A provider that records the request it was handed and then fails, leaving the
/// generation task running with a frozen request in its checkpoint.
struct FailingService {
    requests: std::sync::Mutex<Vec<ModelRequest>>,
}

impl ion_ai::ModelService for FailingService {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> ion_ai::BoxFuture<'a, Result<ModelStream, ProviderError>> {
        Box::pin(async move {
            self.requests.lock().expect("request mutex").push(request);
            Err(ProviderError {
                kind: ProviderErrorKind::Transport,
                message: "the provider went away".to_owned(),
            })
        })
    }
}

fn catalog_with(tool: Option<Lost>) -> Arc<ToolCatalog> {
    let mut catalog = ToolCatalog::new();
    if let Some(tool) = tool {
        catalog.register(tool).expect("register lost");
    }
    Arc::new(catalog)
}

/// A registry that can run a tool chain, with the tool kind optional so a test
/// can hold dispatch until it is ready to observe the interruption.
fn registry(
    service: Arc<dyn ion_ai::ModelService>,
    catalog: Arc<ToolCatalog>,
    tools: bool,
) -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    registry
        .register(
            kind(ion_core::builtin::GENERATION),
            1,
            Arc::new(GenerationKind::new(model_ref(), service, catalog.clone())),
        )
        .expect("generation");
    registry
        .register(
            kind(ion_core::builtin::POST_TOOLS),
            1,
            Arc::new(PostToolsKind),
        )
        .expect("post-tools");
    if tools {
        registry
            .register(
                kind(ion_core::builtin::TOOL),
                1,
                Arc::new(ToolKind::new(catalog)),
            )
            .expect("tool");
    }
    registry
}

fn submission(target: ConversationId, text: &str) -> InputRequest {
    InputRequest {
        target,
        sender: InputSender::User,
        mode: InputMode::Submit,
        request_key: Some(RequestKey::new("turn-1").expect("request key")),
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

/// Locate the one task of a kind, so the raw write can target its row.
async fn only(driver: &TaskDriver, task_kind: &str) -> TaskId {
    let snapshot = driver.snapshot().await;
    let mut found = snapshot
        .tasks
        .iter()
        .filter(|task| task.kind == kind(task_kind))
        .map(|task| task.id);
    let id = found.next().expect("the chain must create this task");
    assert!(found.next().is_none(), "expected exactly one {task_kind}");
    id
}

/// Overwrite a task's stored checkpoint behind a closed session, the way a
/// damaged database or an older build would present it.
fn damage_checkpoint(path: &Path, task: TaskId, checkpoint: &str) {
    let connection = rusqlite::Connection::open(path).expect("open raw");
    let updated = connection
        .execute(
            "UPDATE tasks SET checkpoint = ?2 WHERE id = ?1",
            rusqlite::params![task.get(), checkpoint],
        )
        .expect("write the checkpoint");
    assert_eq!(updated, 1, "the task row must exist");
}

/// Drive one tool call until its invocation is lost after the call landed, then
/// close. What is left behind is a running task with a recorded dispatch.
async fn dispatched_call(db: &Path, retry_safe: bool) -> (TaskId, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = Arc::new(ScriptedModelService::new([Script::Stream(vec![
        completed(assistant_tool_call("call-1", "lost")),
    ])]));
    let driver = TaskDriver::create(
        db,
        registry(
            service,
            catalog_with(Some(Lost {
                calls: calls.clone(),
                lost: Arc::new(AtomicBool::new(true)),
                retry_safe,
            })),
            false,
        ),
    )
    .expect("create");
    let root = driver.snapshot().await.root_conversation;
    let submitted = driver
        .submit_input(submission(root, "make it happen"), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("a submission binds its turn root");
    driver.drive_task(turn).await.expect("drive the turn");
    let tool = only(&driver, ion_core::builtin::TOOL).await;

    // The tool kind is registered late, so the dispatch happens under test.
    driver
        .register_task_kind(
            kind(ion_core::builtin::TOOL),
            1,
            Arc::new(ToolKind::new(catalog_with(Some(Lost {
                calls: calls.clone(),
                lost: Arc::new(AtomicBool::new(true)),
                retry_safe,
            })))),
        )
        .expect("register the tool kind");
    let outcome = driver.drive_task(tool).await.expect("dispatch the call");
    assert!(
        matches!(outcome, DriveOutcome::Interrupted(_)),
        "the lost invocation is not a terminal outcome"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        driver.task(tool).await.expect("tool record").status,
        TaskStatus::Running
    );

    driver.close(CloseMode::Graceful).await;
    drop(driver);
    (tool, calls)
}

/// Drive a running tool to settlement after reopen.
async fn recover_tool(
    db: &Path,
    tool: TaskId,
    catalog: Arc<ToolCatalog>,
    calls: &Arc<AtomicUsize>,
) -> (TaskOutcomeKind, String, usize) {
    let service = Arc::new(ScriptedModelService::new([Script::Stream(vec![
        completed(assistant_text("done")),
    ])]));
    let driver = TaskDriver::open(db, registry(service, catalog, true)).expect("reopen");
    let DriveOutcome::Settled(settlement) =
        driver.drive_task(tool).await.expect("recover the tool")
    else {
        panic!("recovery must settle: a damaged checkpoint is not retried forever");
    };
    let status = settlement.outcome.value["result"]["error"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let count = calls.load(Ordering::SeqCst);
    driver.close(CloseMode::Graceful).await;
    drop(driver);
    (settlement.outcome.kind, status, count)
}

/// A checkpoint that exists but cannot be read is not an absence of dispatch:
/// reading it as one would repeat a call that may already have landed.
#[tokio::test]
async fn a_damaged_dispatch_record_is_not_read_as_never_dispatched() {
    for (index, damaged) in [
        "{\"dispatch\": 42}",
        // The pre-freeze shape, from a build that did not record the policy.
        "{\"dispatch\":{\"call_id\":\"call-1\",\"name\":\"lost\",\"attempts\":1}}",
        // Evidence about a different call.
        "{\"dispatch\":{\"call_id\":\"call-2\",\"name\":\"lost\",\"attempts\":1,\"retry_safe\":true}}",
    ]
    .into_iter()
    .enumerate()
    {
        let db = TempDb::new(&format!("recovery-damaged-{index}"));
        let (tool, calls) = dispatched_call(db.path(), false).await;
        damage_checkpoint(db.path(), tool, damaged);
        let (outcome, status, calls) = recover_tool(
            db.path(),
            tool,
            catalog_with(Some(Lost {
                calls: Arc::new(AtomicUsize::new(0)),
                lost: Arc::new(AtomicBool::new(false)),
                retry_safe: true,
            })),
            &calls,
        )
        .await;
        assert_eq!(
            outcome,
            TaskOutcomeKind::Indeterminate,
            "a damaged checkpoint must fail closed for {damaged}"
        );
        assert!(
            status.contains("outcome is unknown"),
            "the result must not claim a known outcome for {damaged}: {status}"
        );
        assert_eq!(calls, 1, "the call must not repeat for {damaged}");
    }
}

/// A checkpoint column that is not JSON at all cannot be reconstructed as any
/// record, so opening refuses the session instead of rebuilding a wrong task.
#[tokio::test]
async fn an_unparseable_checkpoint_refuses_reconstruction() {
    let db = TempDb::new("recovery-unparseable");
    let (tool, _) = dispatched_call(db.path(), false).await;
    damage_checkpoint(db.path(), tool, "not json at all");

    let service = Arc::new(ScriptedModelService::new(Vec::<Script>::new()));
    let error = match TaskDriver::open(db.path(), registry(service, catalog_with(None), true)) {
        Ok(_) => panic!("an unreadable column must not be reconstructed"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("decode failed"),
        "the refusal must name the unreadable column: {error}"
    );
}

#[tokio::test]
async fn a_removed_tool_does_not_resolve_an_uncertain_call() {
    let db = TempDb::new("recovery-removed-tool");
    let (tool, calls) = dispatched_call(db.path(), false).await;

    // The dispatch is recorded, so "not registered" is not a known outcome: it is
    // exactly the case where a removal could hide a landed call.
    let (outcome, status, calls) = recover_tool(db.path(), tool, catalog_with(None), &calls).await;
    assert_eq!(outcome, TaskOutcomeKind::Indeterminate);
    assert!(
        status.contains("outcome is unknown"),
        "a missing tool must not resolve the call: {status}"
    );
    assert_eq!(calls, 1, "the call must not repeat");
}

#[tokio::test]
async fn a_newly_retry_safe_tool_does_not_resolve_an_uncertain_call() {
    let db = TempDb::new("recovery-policy-change");
    let (tool, calls) = dispatched_call(db.path(), false).await;

    // The policy that matters is the one in force when the call was handed over.
    // A tool that becomes retry-safe later must not retroactively make an older
    // uncertain action repeatable.
    let (outcome, status, calls) = recover_tool(
        db.path(),
        tool,
        catalog_with(Some(Lost {
            calls: calls.clone(),
            lost: Arc::new(AtomicBool::new(false)),
            retry_safe: true,
        })),
        &calls,
    )
    .await;
    assert_eq!(outcome, TaskOutcomeKind::Indeterminate);
    assert!(
        status.contains("outcome is unknown"),
        "a changed policy must not resolve the call: {status}"
    );
    assert_eq!(calls, 1, "the call must not repeat");
}

#[tokio::test]
async fn a_damaged_dispatch_record_is_not_reported_as_stopped() {
    let db = TempDb::new("recovery-damaged-abort");
    let (tool, calls) = dispatched_call(db.path(), false).await;
    damage_checkpoint(db.path(), tool, "{\"dispatch\": 42}");

    let service = Arc::new(ScriptedModelService::new(Vec::<Script>::new()));
    let driver =
        TaskDriver::open(db.path(), registry(service, catalog_with(None), true)).expect("reopen");
    let turn = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.kind == kind(ion_core::builtin::GENERATION))
        .expect("turn root")
        .id;
    driver.cancel_turn(turn).await.expect("cancel the turn");
    let DriveOutcome::Settled(settlement) = driver.drive_task(tool).await.expect("abort") else {
        panic!("the cancelled call must settle");
    };
    let outcome = &settlement.outcome;
    assert_eq!(
        outcome.kind,
        TaskOutcomeKind::Indeterminate,
        "abort must not claim a damaged dispatch never happened"
    );
    assert!(
        outcome.value["result"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("outcome is unknown")),
        "the result must not claim the call was stopped: {:?}",
        outcome.value
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    driver.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn a_damaged_model_request_is_neither_rebuilt_nor_dispatched() {
    let db = TempDb::new("recovery-damaged-request");
    let requests = Arc::new(FailingService {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let driver = TaskDriver::create(
        db.path(),
        registry(requests.clone(), catalog_with(None), true),
    )
    .expect("create");
    let root = driver.snapshot().await.root_conversation;
    let submitted = driver
        .submit_input(submission(root, "answer me"), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("a submission binds its turn root");
    assert!(
        matches!(
            driver.drive_task(turn).await.expect("drive the turn"),
            DriveOutcome::Interrupted(_)
        ),
        "a provider failure leaves the attempt recoverable"
    );
    assert_eq!(requests.requests.lock().expect("request mutex").len(), 1);
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    // The request is recorded but unreadable, so recovery cannot know what was
    // sent. Rebuilding it from current history would dispatch a different one.
    damage_checkpoint(db.path(), turn, "{\"request\": \"truncated\"}");
    let reopened = TaskDriver::open(
        db.path(),
        registry(requests.clone(), catalog_with(None), true),
    )
    .expect("reopen");
    let DriveOutcome::Settled(settlement) = reopened.drive_task(turn).await.expect("recover")
    else {
        panic!("a damaged request must settle rather than retry forever");
    };
    assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Indeterminate);
    assert_eq!(
        requests.requests.lock().expect("request mutex").len(),
        1,
        "recovery must not dispatch a rebuilt request"
    );
    reopened.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn an_intact_recorded_request_is_still_replayed_on_recovery() {
    // The positive control for the failing service above: an undamaged checkpoint
    // is still reused, so the fail-closed path did not swallow ordinary recovery.
    let db = TempDb::new("recovery-intact-request");
    let requests = Arc::new(FailingService {
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let driver = TaskDriver::create(
        db.path(),
        registry(requests.clone(), catalog_with(None), true),
    )
    .expect("create");
    let root = driver.snapshot().await.root_conversation;
    let submitted = driver
        .submit_input(submission(root, "answer me"), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("a submission binds its turn root");
    driver.drive_task(turn).await.expect("drive the turn");
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    let reopened = TaskDriver::open(
        db.path(),
        registry(requests.clone(), catalog_with(None), true),
    )
    .expect("reopen");
    reopened.drive_task(turn).await.expect("recover");
    {
        let recorded = requests.requests.lock().expect("request mutex");
        assert_eq!(recorded.len(), 2, "recovery replays the recorded request");
        assert_eq!(
            recorded[0].messages, recorded[1].messages,
            "the replayed request is the recorded one, not a rebuilt one"
        );
    }
    reopened.close(CloseMode::Graceful).await;
}

const CRASH_DB: &str = "ION_K5_CRASH_DB";
const CRASH_WITNESS: &str = "ION_K5_CRASH_WITNESS";

/// A tool whose effect is observable outside the process: it appends one line to
/// a witness file and then loses its invocation to a real process death.
struct Witnessed {
    path: PathBuf,
}

impl Tool for Witnessed {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "witnessed".to_owned(),
            description: "records an external effect and dies".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    fn call<'a>(&'a self, _arguments: Value) -> ToolFuture<'a> {
        let path = self.path.clone();
        Box::pin(async move {
            use std::io::Write;
            let mut witness = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .expect("open the witness");
            writeln!(witness, "effect").expect("write the witness");
            witness.sync_all().expect("flush the witness");
            // The dispatch is already durable; dying here is the crash window
            // this test is about.
            std::process::abort();
        })
    }
}

/// Runs only inside the child process spawned by the test below. It creates the
/// session, drives one tool call to the point where the effect has landed, and
/// dies inside the call.
#[tokio::test]
async fn dispatch_window_child() {
    let Some(path) = std::env::var_os(CRASH_DB) else {
        return;
    };
    let witness = std::env::var_os(CRASH_WITNESS).expect("witness path");
    let service = Arc::new(ScriptedModelService::new([Script::Stream(vec![
        completed(assistant_tool_call("call-1", "witnessed")),
    ])]));
    let mut catalog = ToolCatalog::new();
    catalog
        .register(Witnessed {
            path: PathBuf::from(witness),
        })
        .expect("register the witnessed tool");
    let driver = TaskDriver::create(Path::new(&path), registry(service, Arc::new(catalog), true))
        .expect("create");
    let root = driver.snapshot().await.root_conversation;
    let submitted = driver
        .submit_input(submission(root, "cause an effect"), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("a submission binds its turn root");
    // This drive does not return: the tool aborts the process.
    let _ = driver.drive_task(turn).await;
    panic!("the witnessed call must not return");
}

fn witness_lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|contents| contents.lines().count())
        .unwrap_or_default()
}

/// The dispatch window is the interval between the durable dispatch record and
/// the call's settlement. A process that dies inside it leaves an uncertain
/// external effect, and recovery must neither repeat it nor report it as known.
#[tokio::test]
async fn a_process_death_in_the_dispatch_window_does_not_repeat_the_call() {
    let db = TempDb::new("recovery-dispatch-window");
    let witness = db.path().with_extension("witness");
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "dispatch_window_child", "--nocapture"])
        .env(CRASH_DB, db.path())
        .env(CRASH_WITNESS, &witness)
        .status()
        .expect("spawn the crashing child");
    assert!(
        !status.success(),
        "the child must die inside the call, not return: {status:?}"
    );
    assert_eq!(
        witness_lines(&witness),
        1,
        "the external effect landed exactly once before the crash"
    );

    // The parent now owns the database: the dead process released its lock.
    let service = Arc::new(ScriptedModelService::new([Script::Stream(vec![
        completed(assistant_text("done")),
    ])]));
    let mut catalog = ToolCatalog::new();
    catalog
        .register(Witnessed {
            path: witness.clone(),
        })
        .expect("register the witnessed tool");
    let driver = TaskDriver::open(db.path(), registry(service, Arc::new(catalog), true))
        .expect("reopen the crashed session");
    let tool = only(&driver, ion_core::builtin::TOOL).await;
    assert_eq!(
        driver.task(tool).await.expect("tool record").status,
        TaskStatus::Running,
        "the crashed invocation is still running, so it needs an explicit recovery"
    );

    let DriveOutcome::Settled(settlement) = driver.drive_task(tool).await.expect("recover") else {
        panic!("recovery must settle");
    };
    assert_eq!(
        settlement.outcome.kind,
        TaskOutcomeKind::Indeterminate,
        "the crash window is an uncertain external effect"
    );
    assert_eq!(
        witness_lines(&witness),
        1,
        "recovery must not repeat the external effect"
    );
    assert!(
        settlement.outcome.value["result"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("outcome is unknown")),
        "the result must not claim a known outcome: {:?}",
        settlement.outcome.value
    );
    driver.close(CloseMode::Graceful).await;
    let _ = std::fs::remove_file(&witness);
}
