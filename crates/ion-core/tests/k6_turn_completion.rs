//! K6: a completed turn records which member closed it.
//!
//! The member that ends a chain is only decided while the chain runs, so a
//! joiner cannot name it in advance. The durable receipt — written with the
//! settlement that closes the turn and the slot release — is what makes "you
//! may now depend on the worker's answer" expressible, and `wait_turn` is the
//! client-side form of it.

use std::sync::Arc;
use std::time::Duration;

use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::builtin::{Builtins, Tool, ToolCatalog, ToolFuture, WORKER};
use ion_core::{
    CloseMode, ConversationId, DriveOutcome, InputBody, InputMode, InputRequest, InputSender,
    RequestKey, Session, SessionError, TaskDriver, TaskDriverError, TaskId, TaskKindName,
    TaskRegistry, TaskRequest, TaskStatus, TurnTemplate,
};
use serde_json::json;
use tokio::sync::Notify;

mod support;

use support::TempDb;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

/// A tool that blocks until released, so a chain can be observed in flight.
struct Gated {
    name: String,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl Tool for Gated {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: "block until released".to_owned(),
            input_schema: json!({"type": "object"}),
        }
    }

    fn call<'a>(&'a self, _arguments: serde_json::Value) -> ToolFuture<'a> {
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Ok(json!({"released": true}))
        })
    }
}

struct Gate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }
}

fn assistant_tool_call(id: &str, tool: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(ToolCall {
            id: id.to_owned(),
            name: tool.to_owned(),
            arguments: json!({}),
        })],
        provider_replay: None,
    }
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

/// One worker chain: generation -> gated tool -> join -> continuation.
fn chain_scripts(tool: &str) -> Vec<Script> {
    vec![
        Script::Stream(vec![completed(assistant_tool_call("call-1", tool))]),
        Script::Stream(vec![completed(assistant_text("worker answer"))]),
    ]
}

struct Harness {
    driver: TaskDriver,
    gates: [Gate; 2],
}

impl Harness {
    fn new(scripts: impl IntoIterator<Item = Script>) -> Self {
        let gates = [Gate::new(), Gate::new()];
        let mut catalog = ToolCatalog::new();
        for (index, gate) in gates.iter().enumerate() {
            catalog
                .register(Gated {
                    name: format!("gate-{index}"),
                    entered: gate.entered.clone(),
                    release: gate.release.clone(),
                })
                .expect("register gate");
        }
        let mut registry = TaskRegistry::new();
        Builtins {
            model: ModelRef {
                provider: "test".to_owned(),
                model: "scripted".to_owned(),
            },
            service: Arc::new(ScriptedModelService::new(scripts)),
            tools: Arc::new(catalog),
        }
        .register(&mut registry)
        .expect("register built-ins");
        Self {
            driver: TaskDriver::new(Session::new().expect("session"), registry)
                .with_turn_template(TurnTemplate::new(kind(ion_core::builtin::GENERATION))),
            gates,
        }
    }

    /// Spawn a worker whose first tool call blocks, and return its turn root.
    async fn spawn(&self, brief: &str, gate: usize) -> (ConversationId, TaskId) {
        let root = self.driver.snapshot().await.root_conversation;
        let spawn = self
            .driver
            .create_turn(TaskRequest {
                conversation_id: root,
                kind: kind(WORKER),
                schema_version: 1,
                input: json!({"brief": brief}),
                dependencies: Vec::new(),
            })
            .await
            .expect("spawn turn");
        assert!(matches!(
            self.driver.drive_task(spawn.task_id).await.expect("spawn"),
            DriveOutcome::Settled(_)
        ));
        let worker = self
            .driver
            .owned_conversations(spawn.task_id)
            .await
            .expect("spawn task")
            .pop()
            .expect("one owned worker");
        tokio::time::timeout(Duration::from_secs(10), self.gates[gate].entered.notified())
            .await
            .expect("the worker's tool call must start");
        let turn = self
            .driver
            .conversation(worker)
            .await
            .expect("worker record")
            .foreground_turn
            .expect("the worker holds its own turn");
        (worker, turn)
    }

    fn open(&self, gate: usize) {
        self.gates[gate].release.notify_one();
    }

    /// Wait until the receipt exists, then return it.
    async fn closed_by(&self, turn: TaskId) -> TaskId {
        tokio::time::timeout(Duration::from_secs(10), self.driver.wait_turn(turn))
            .await
            .expect("the turn must close")
            .expect("wait for the turn")
    }
}

#[tokio::test]
async fn a_turn_records_the_member_that_closed_it() {
    let harness = Harness::new(chain_scripts("gate-0"));
    let (worker, turn) = harness.spawn("first", 0).await;

    // A generation settles as soon as it has planned its children, so the
    // receipt is not a property of the root's own settlement.
    let root_record = harness.driver.task(turn).await.expect("root");
    assert!(matches!(root_record.status, TaskStatus::Terminal(_)));
    assert_eq!(
        root_record.turn_closed_by, None,
        "the turn is still running while its tool is in flight"
    );
    assert_eq!(harness.driver.turn_closed_by(turn).await, None);

    harness.open(0);
    let closed_by = harness.closed_by(turn).await;

    // The closing member is the continuation generation, not the root.
    assert_ne!(closed_by, turn);
    let closing = harness
        .driver
        .task(closed_by)
        .await
        .expect("closing member");
    assert_eq!(closing.kind, kind(ion_core::builtin::GENERATION));
    assert_eq!(closing.turn, Some(turn));
    let TaskStatus::Terminal(outcome) = &closing.status else {
        panic!("the closing member must be terminal");
    };
    assert_eq!(outcome.value["text"], json!("worker answer"));
    // The root keeps its own outcome, unchanged by the closure.
    let root_outcome = harness.driver.task(turn).await.expect("root");
    let TaskStatus::Terminal(outcome) = &root_outcome.status else {
        panic!("root terminal");
    };
    assert_eq!(outcome.value["tool_calls"], json!(1));
    assert_eq!(
        harness.driver.turn_closed_by(turn).await,
        Some(closed_by),
        "the receipt is stable once written"
    );
    assert_eq!(
        harness
            .driver
            .conversation(worker)
            .await
            .expect("worker")
            .foreground_turn,
        None
    );
}

#[tokio::test]
async fn a_later_turn_does_not_reopen_a_closed_one() {
    let mut scripts = chain_scripts("gate-0");
    scripts.push(Script::Stream(vec![completed(assistant_text(
        "follow-up answer",
    ))]));
    let harness = Harness::new(scripts);
    let (worker, first) = harness.spawn("first", 0).await;
    harness.open(0);
    let closed_by = harness.closed_by(first).await;

    // A follow-up runs as its own turn and cannot change the earlier receipt.
    let follow_up = harness
        .driver
        .admit_input(InputRequest {
            target: worker,
            sender: InputSender::User,
            mode: InputMode::FollowUp,
            request_key: Some(RequestKey::new("follow-up").expect("key")),
            body: InputBody::Text("second".to_owned()),
        })
        .await
        .expect("follow-up");
    // The first turn is already closed, so the worker is idle and the follow-up
    // opens its own turn; the point is that the earlier receipt is unaffected.
    let _ = follow_up;
    let _ = harness
        .driver
        .schedule_next_turn(worker)
        .await
        .expect("schedule");
    let second = harness
        .driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| {
            task.conversation_id == worker && task.turn == Some(task.id) && task.id != first
        })
        .map(|task| task.id)
        .expect("the follow-up opened its own turn");
    // Admission opens a turn; the client drives it.
    harness
        .driver
        .drive_task(second)
        .await
        .expect("drive the follow-up turn");
    let second_closed_by = harness.closed_by(second).await;

    assert_ne!(second, first);
    assert_eq!(
        harness.driver.turn_closed_by(first).await,
        Some(closed_by),
        "the first turn's receipt is frozen"
    );
    assert_ne!(second_closed_by, closed_by);
    assert_eq!(
        harness.driver.turn_closed_by(second).await,
        Some(second_closed_by)
    );
}

#[tokio::test]
async fn waiting_for_one_turn_does_not_block_another() {
    // A is spawned and parks in its gate before B is spawned, so the model calls
    // arrive as A's call, B's call, then each continuation in release order.
    let harness = Harness::new([
        Script::Stream(vec![completed(assistant_tool_call("call-a", "gate-0"))]),
        Script::Stream(vec![completed(assistant_tool_call("call-b", "gate-1"))]),
        Script::Stream(vec![completed(assistant_text("answer b"))]),
        Script::Stream(vec![completed(assistant_text("answer a"))]),
    ]);
    let (_first_worker, first) = harness.spawn("first", 0).await;
    let (second_worker, second) = harness.spawn("second", 1).await;

    let waiter = {
        let driver = harness.driver.clone();
        tokio::spawn(async move { driver.wait_turn(first).await })
    };
    // Releasing the other worker's gate closes its turn while the first wait is
    // still pending, so the wait holds nothing shared.
    harness.open(1);
    let second_closed_by = harness.closed_by(second).await;
    assert!(!waiter.is_finished(), "the first turn is still open");

    harness.open(0);
    let first_closed_by = harness.closed_by(first).await;
    let waited = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("the wait must resolve")
        .expect("join")
        .expect("wait");
    assert_eq!(waited, first_closed_by);
    assert_ne!(first_closed_by, second_closed_by);
    assert_eq!(
        harness
            .driver
            .conversation(second_worker)
            .await
            .expect("record")
            .foreground_turn,
        None
    );
}

#[tokio::test]
async fn a_cancelled_turn_still_records_its_closure() {
    let harness = Harness::new(chain_scripts("gate-0"));
    let (_worker, turn) = harness.spawn("first", 0).await;

    harness
        .driver
        .cancel_turn(turn)
        .await
        .expect("cancel the worker's turn");
    // A mark is not completion; the cleanup settlement is.
    let closed_by = harness.closed_by(turn).await;
    let closing = harness
        .driver
        .task(closed_by)
        .await
        .expect("closing member");
    let TaskStatus::Terminal(outcome) = &closing.status else {
        panic!("the closing member must be terminal");
    };
    assert_eq!(closing.turn, Some(turn));
    assert_eq!(
        outcome.kind,
        ion_core::TaskOutcomeKind::Indeterminate,
        "the gated call was dispatched, so its cleanup records uncertainty"
    );
    // A mark alone is not completion; the receipt exists because cleanup settled.
    assert_eq!(harness.driver.turn_closed_by(turn).await, Some(closed_by));
}

#[tokio::test]
async fn waiting_for_a_task_that_is_not_a_turn_root_is_refused() {
    let harness = Harness::new(chain_scripts("gate-0"));
    let (_worker, turn) = harness.spawn("first", 0).await;
    let member = harness
        .driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.turn == Some(turn) && task.id != turn)
        .map(|task| task.id)
        .expect("a turn member");
    let error = harness
        .driver
        .wait_turn(member)
        .await
        .expect_err("a turn member is not a turn root");
    assert!(matches!(
        error,
        TaskDriverError::Session(SessionError::NotATurnRoot(id)) if id == member
    ));
}

#[tokio::test]
async fn a_turn_receipt_survives_reopen() {
    let db = TempDb::new("k6-turn-completion");
    let mut session = Session::create(db.path()).expect("create");
    let root = session.root_conversation();
    let spawn = session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind(WORKER),
            schema_version: 1,
            input: json!({"brief": "first"}),
            dependencies: Vec::new(),
        })
        .expect("spawn task");
    let driver = TaskDriver::new(
        session,
        registry_with([Script::Stream(vec![completed(assistant_text("answer"))])]),
    );
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
    // The scripted worker answers immediately, so its turn is already closed;
    // find the root and read the receipt.
    let turn = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.conversation_id == worker && task.turn == Some(task.id))
        .map(|task| task.id)
        .expect("the worker's turn root");
    let closed_by = tokio::time::timeout(Duration::from_secs(10), driver.wait_turn(turn))
        .await
        .expect("must close")
        .expect("wait");
    let before = driver.snapshot().await;
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    let reopened = TaskDriver::open(db.path(), registry_with([] as [Script; 0])).expect("open");
    assert_eq!(reopened.snapshot().await, before);
    assert_eq!(reopened.turn_closed_by(turn).await, Some(closed_by));
    assert_eq!(
        reopened.task(turn).await.expect("root").turn_closed_by,
        Some(closed_by)
    );
}

/// The production kinds over a scripted model.
fn registry_with(scripts: impl IntoIterator<Item = Script>) -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    Builtins {
        model: ModelRef {
            provider: "test".to_owned(),
            model: "scripted".to_owned(),
        },
        service: Arc::new(ScriptedModelService::new(scripts)),
        tools: Arc::new(ToolCatalog::new()),
    }
    .register(&mut registry)
    .expect("register built-ins");
    registry
}
