//! K6: a worker's run is an independent turn in its own conversation.
//!
//! Worker-local turn scope means a worker is never idle while it works: a
//! follow-up queues behind the running chain instead of starting a second one
//! beside it, the worker's own work can be stopped as a unit, and the creator's
//! turn scope stays independent of it.

use std::sync::Arc;
use std::time::Duration;

use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::builtin::{Builtins, POST_TOOLS, TOOL, Tool, ToolCatalog, ToolFuture, WORKER};
use ion_core::{
    AdmissionReceipt, ConversationId, DriveOutcome, InputBody, InputDisposition, InputMode,
    InputRequest, InputSender, PlannedTarget, PlannedTurn, RequestKey, Session, SessionError,
    TaskCompletion, TaskDriver, TaskDriverError, TaskId, TaskKindName, TaskRegistry, TaskRequest,
    TaskStatus, TurnTemplate,
};
use serde_json::json;
use tokio::sync::Notify;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

/// Blocks until the test releases it, so a worker chain can be observed in
/// flight. Each gate is its own tool so two workers never share a signal.
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

    fn tool(&self, name: &str) -> Gated {
        Gated {
            name: name.to_owned(),
            entered: self.entered.clone(),
            release: self.release.clone(),
        }
    }

    fn open(&self) {
        self.release.notify_one();
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

fn worker_scripts(tool: &str) -> Vec<Script> {
    vec![
        Script::Stream(vec![completed(assistant_tool_call("call-1", tool))]),
        Script::Stream(vec![completed(assistant_text("worker answer"))]),
    ]
}

/// One spawned worker, with the members of its chain.
struct Spawned {
    worker: ConversationId,
    /// The worker's own turn root (its initial generation).
    root: TaskId,
    tool: TaskId,
    join: TaskId,
}

struct Harness {
    driver: TaskDriver,
    service: Arc<ScriptedModelService>,
    gates: [Gate; 2],
}

impl Harness {
    fn new(scripts: impl IntoIterator<Item = Script>) -> Self {
        let gates = [Gate::new(), Gate::new()];
        let mut catalog = ToolCatalog::new();
        catalog.register(gates[0].tool("gate-a")).expect("gate a");
        catalog.register(gates[1].tool("gate-b")).expect("gate b");
        let service = Arc::new(ScriptedModelService::new(scripts));
        let mut registry = TaskRegistry::new();
        Builtins {
            model: ModelRef {
                provider: "test".to_owned(),
                model: "scripted".to_owned(),
            },
            service: service.clone(),
            tools: Arc::new(catalog),
        }
        .register(&mut registry)
        .expect("register built-ins");
        Self {
            // A conversation answers queued input with a generation turn; that is
            // what drains the follow-up behind a busy worker.
            driver: TaskDriver::new(Session::new().expect("session"), registry)
                .with_turn_template(TurnTemplate::new(kind(ion_core::builtin::GENERATION))),
            service,
            gates,
        }
    }

    /// Spawn a worker whose first tool call blocks on `gate`.
    async fn spawn(&self, brief: &str, gate: usize) -> Spawned {
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

        let snapshot = self.driver.snapshot().await;
        let in_worker = |name: &str| {
            let name = kind(name);
            snapshot
                .tasks
                .iter()
                .find(|task| task.conversation_id == worker && task.kind == name)
                .map(|task| task.id)
                .expect("chain member")
        };
        let root_task = snapshot
            .tasks
            .iter()
            .find(|task| task.conversation_id == worker && task.turn == Some(task.id))
            .map(|task| task.id)
            .expect("the worker's own turn root");
        Spawned {
            worker,
            root: root_task,
            tool: in_worker(TOOL),
            join: in_worker(POST_TOOLS),
        }
    }

    /// Release the gate and wait for the whole chain to settle, returning the
    /// continuation generation that ended it.
    async fn finish(&self, spawned: &Spawned, gate: usize) -> TaskId {
        self.gates[gate].open();
        self.wait(spawned.tool).await;
        self.wait(spawned.join).await;
        let continuation = self
            .driver
            .snapshot()
            .await
            .tasks
            .iter()
            .find(|task| {
                task.conversation_id == spawned.worker
                    && task.turn == Some(spawned.root)
                    && task.id != spawned.root
                    && task.kind == kind(ion_core::builtin::GENERATION)
            })
            .map(|task| task.id)
            .expect("the join starts a continuation generation");
        self.wait(continuation).await;
        continuation
    }

    async fn wait(&self, task_id: TaskId) {
        tokio::time::timeout(Duration::from_secs(10), self.driver.wait_task(task_id))
            .await
            .expect("the chain must not stall")
            .expect("wait");
    }

    async fn slot(&self, conversation: ConversationId) -> Option<TaskId> {
        self.driver
            .conversation(conversation)
            .await
            .expect("conversation record")
            .foreground_turn
    }

    async fn admit(
        &self,
        worker: ConversationId,
        key: &str,
        mode: InputMode,
        text: &str,
    ) -> AdmissionReceipt {
        self.driver
            .admit_input(InputRequest {
                target: worker,
                sender: InputSender::User,
                mode,
                request_key: Some(RequestKey::new(key).expect("request key")),
                body: InputBody::Text(text.to_owned()),
            })
            .await
            .expect("admit input")
    }
}

#[tokio::test]
async fn a_worker_run_occupies_its_own_conversation_slot() {
    let harness = Harness::new([
        Script::Stream(vec![completed(assistant_tool_call("call-1", "gate-a"))]),
        Script::Stream(vec![completed(assistant_text("worker answer"))]),
    ]);
    let spawned = harness.spawn("do the thing", 0).await;

    // The worker is busy in its own conversation and only there: the creator's
    // conversation is idle again, because the worker's run is not a member of
    // the creator's turn.
    let snapshot = harness.driver.snapshot().await;
    let root = snapshot.root_conversation;
    assert_eq!(
        snapshot
            .conversations
            .iter()
            .find(|conversation| conversation.id == root)
            .expect("root")
            .foreground_turn,
        None,
        "the creator's conversation is idle again"
    );
    assert_eq!(
        harness.slot(spawned.worker).await,
        Some(spawned.root),
        "the worker's run holds the worker's own slot"
    );

    // The turn spans the chain: the root settled as soon as it planned its tool,
    // and the tool still holds the slot.
    let snapshot = harness.driver.snapshot().await;
    let member = |id: TaskId| {
        snapshot
            .tasks
            .iter()
            .find(|task| task.id == id)
            .expect("member")
            .clone()
    };
    assert_eq!(member(spawned.root).turn, Some(spawned.root));
    assert!(
        matches!(member(spawned.root).status, TaskStatus::Terminal(_)),
        "a generation settles before its tools finish"
    );
    assert_eq!(member(spawned.tool).status, TaskStatus::Running);

    // Cancelling the worker's turn stops exactly that run.
    harness
        .driver
        .cancel_turn(spawned.root)
        .await
        .expect("cancel the worker's turn");
    harness.wait(spawned.tool).await;
    let snapshot = harness.driver.snapshot().await;
    assert!(
        snapshot
            .tasks
            .iter()
            .filter(|task| task.conversation_id == spawned.worker)
            .all(|task| matches!(task.status, TaskStatus::Terminal(_))),
        "the worker's whole chain settled"
    );
    assert_eq!(
        snapshot
            .conversations
            .iter()
            .find(|conversation| conversation.id == spawned.worker)
            .expect("worker")
            .foreground_turn,
        None,
        "cleanup released the worker's slot"
    );
}

#[tokio::test]
async fn a_follow_up_to_a_running_worker_queues_and_runs_after_it() {
    let mut scripts = worker_scripts("gate-a");
    scripts.push(Script::Stream(vec![completed(assistant_text(
        "follow-up answer",
    ))]));
    let harness = Harness::new(scripts);
    let spawned = harness.spawn("first", 0).await;

    // A follow-up while the worker is busy queues instead of overlapping.
    let follow_up = harness
        .admit(spawned.worker, "follow-up", InputMode::FollowUp, "second")
        .await;
    assert!(
        !follow_up.started_turn(),
        "a busy worker queues the follow-up"
    );
    let roots = |tasks: &[ion_core::TaskRecord]| -> Vec<TaskId> {
        tasks
            .iter()
            .filter(|task| task.conversation_id == spawned.worker && task.turn == Some(task.id))
            .map(|task| task.id)
            .collect()
    };
    assert_eq!(
        roots(&harness.driver.snapshot().await.tasks),
        vec![spawned.root],
        "no second turn was opened beside the running chain"
    );

    // Finishing the first run drains the follow-up into its own turn. Asking
    // again is idempotent, and keeps this test independent of whether the
    // settlement's own drain had finished when its wait resolved.
    harness.finish(&spawned, 0).await;
    let _ = harness
        .driver
        .schedule_next_turn(spawned.worker)
        .await
        .expect("schedule");
    let snapshot = harness.driver.snapshot().await;
    let after: Vec<TaskId> = roots(&snapshot.tasks);
    assert_eq!(after.len(), 2, "the follow-up ran as its own turn");
    let second = *after
        .iter()
        .find(|id| **id != spawned.root)
        .expect("second root");
    harness.wait(second).await;

    // The follow-up's generation saw the finished first run, so the two ran in
    // order rather than side by side.
    let requests = harness.service.requests();
    assert_eq!(requests.len(), 3, "one generation per turn, in order");
    let roles: Vec<Role> = requests[2]
        .messages
        .iter()
        .map(|message| message.role)
        .collect();
    assert_eq!(
        roles,
        vec![
            Role::User,
            Role::Assistant,
            Role::Tool,
            Role::Assistant,
            Role::User
        ]
    );
    assert!(matches!(
        harness
            .driver
            .snapshot()
            .await
            .inputs
            .iter()
            .find(|input| input.id == follow_up.input_id)
            .expect("follow-up")
            .disposition,
        InputDisposition::Consumed(_)
    ));
}

#[tokio::test]
async fn cancelling_one_worker_leaves_its_sibling_and_creator_alone() {
    let mut scripts = worker_scripts("gate-a");
    scripts.extend(worker_scripts("gate-b"));
    let harness = Harness::new(scripts);
    let first = harness.spawn("first", 0).await;
    harness.finish(&first, 0).await;
    let second = harness.spawn("second", 1).await;

    harness
        .driver
        .cancel_turn(second.root)
        .await
        .expect("cancel the second worker's turn");
    harness.wait(second.tool).await;

    let snapshot = harness.driver.snapshot().await;
    let all_terminal = |conversation: ConversationId| {
        snapshot
            .tasks
            .iter()
            .filter(|task| task.conversation_id == conversation)
            .all(|task| matches!(task.status, TaskStatus::Terminal(_)))
    };
    assert!(all_terminal(second.worker), "the cancelled worker settled");
    assert!(
        all_terminal(first.worker),
        "the sibling worker is untouched"
    );
    assert_eq!(harness.slot(second.worker).await, None);
    assert_eq!(harness.slot(first.worker).await, None);
    // The cancelled tool did not invent a result for an un-dispatched effect.
    let cancelled: Vec<_> = snapshot
        .tasks
        .iter()
        .filter(|task| task.conversation_id == second.worker)
        .filter_map(|task| match &task.status {
            TaskStatus::Terminal(outcome) => Some(outcome.kind),
            _ => None,
        })
        .collect();
    assert!(!cancelled.is_empty());
}

#[tokio::test]
async fn a_plan_cannot_open_a_turn_that_is_already_held() {
    struct OpenTurn {
        target: ConversationId,
    }

    impl ion_core::TaskKind for OpenTurn {
        fn execute<'a>(
            &'a self,
            _task: ion_core::RunningTask,
            _context: ion_core::TaskContext,
        ) -> ion_core::TaskFuture<'a> {
            Box::pin(async move {
                let mut plan = ion_core::TaskPlan::new();
                plan.create_task(ion_core::PlannedTask {
                    conversation_id: PlannedTarget::existing(self.target),
                    kind: kind(WORKER),
                    schema_version: 1,
                    input: json!({}),
                    dependencies: Vec::new(),
                    turn: PlannedTurn::Own,
                });
                Ok(TaskCompletion::completed(json!("opened")).with_plan(plan))
            })
        }

        fn recover<'a>(
            &'a self,
            task: ion_core::RunningTask,
            context: ion_core::TaskContext,
        ) -> ion_core::TaskFuture<'a> {
            self.execute(task, context)
        }

        fn abort<'a>(
            &'a self,
            _task: ion_core::RunningTask,
            _context: ion_core::AbortContext,
        ) -> ion_core::TaskFuture<'a> {
            Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
        }
    }

    let harness = Harness::new([
        Script::Stream(vec![completed(assistant_tool_call("call-1", "gate-a"))]),
        Script::Stream(vec![completed(assistant_text("worker answer"))]),
    ]);
    let spawned = harness.spawn("busy", 0).await;
    let before = harness.driver.snapshot().await;
    harness
        .driver
        .register_task_kind(
            kind("open"),
            1,
            Arc::new(OpenTurn {
                target: spawned.worker,
            }),
        )
        .expect("register");

    let root = before.root_conversation;
    let opener = harness
        .driver
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: kind("open"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .await
        .expect("opener")
        .task_id;
    let error = harness
        .driver
        .drive_task(opener)
        .await
        .expect_err("the slot is already held");
    assert!(
        matches!(
            error,
            TaskDriverError::Session(SessionError::ForegroundTurnBusy(id)) if id == spawned.worker
        ),
        "got {error:?}"
    );

    // The rejected plan rolled back, and the worker's own run is unaffected.
    let snapshot = harness.driver.snapshot().await;
    assert_eq!(
        snapshot
            .tasks
            .iter()
            .filter(|task| task.conversation_id == spawned.worker)
            .count(),
        before
            .tasks
            .iter()
            .filter(|task| task.conversation_id == spawned.worker)
            .count()
    );
    assert_eq!(harness.slot(spawned.worker).await, Some(spawned.root));
    assert_eq!(
        harness.slot(root).await,
        Some(opener),
        "the opener's failed settlement keeps its own turn open and recoverable"
    );
}
