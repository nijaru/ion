use std::sync::Arc;

use ion_ai::{Content, Message, Role, ToolCall};
use ion_core::conversation::context::ContextControl;
use ion_core::{
    AbortContext, ConversationId, DriveOutcome, EntryKind, PlannedEntry, PlannedTask, RunningTask,
    Session, TaskCompletion, TaskContext, TaskDependency, TaskDriver, TaskFuture, TaskKind,
    TaskKindName, TaskOutcomeKind, TaskPlan, TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::json;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

fn futures_pending<F: std::future::Future>(future: &mut std::pin::Pin<Box<F>>) -> bool {
    use std::task::{Context, Poll, Waker};
    !matches!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(_)
    )
}

fn assistant_calls() -> Message {
    Message {
        role: Role::Assistant,
        content: vec![
            Content::ToolCall(ToolCall {
                id: "a".to_owned(),
                name: "read".to_owned(),
                arguments: json!({"path": "a"}),
            }),
            Content::ToolCall(ToolCall {
                id: "b".to_owned(),
                name: "grep".to_owned(),
                arguments: json!({"pattern": "b"}),
            }),
        ],
        provider_replay: None,
    }
}

/// Settles by appending one assistant entry and creating two tool successors
/// plus a join task that depends on both.
struct Generator;

impl TaskKind for Generator {
    fn execute<'a>(&'a self, task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            plan.append_entry(PlannedEntry {
                conversation_id: task.conversation_id,
                kind: EntryKind::new("assistant").expect("entry kind"),
                data: json!({"text": "calling tools"}),
                projection: vec![assistant_calls()],
                context: ContextControl::none(),
            });
            let tool = |plan: &mut TaskPlan, id: &str| {
                plan.create_task(PlannedTask {
                    conversation_id: task.conversation_id,
                    kind: kind("tool"),
                    schema_version: 1,
                    input: json!({"call": id}),
                    dependencies: Vec::new(),
                })
            };
            let first = tool(&mut plan, "a");
            let second = tool(&mut plan, "b");
            plan.create_task(PlannedTask {
                conversation_id: task.conversation_id,
                kind: kind("join"),
                schema_version: 1,
                input: json!({"calls": ["a", "b"]}),
                dependencies: vec![
                    TaskDependency::Planned(first),
                    TaskDependency::Planned(second),
                ],
            });
            Ok(TaskCompletion::completed(json!("dispatched")).with_plan(plan))
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

/// Plans a successor in a conversation that does not exist, so the whole
/// finalization must roll back.
struct Broken;

impl TaskKind for Broken {
    fn execute<'a>(&'a self, task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            plan.append_entry(PlannedEntry {
                conversation_id: task.conversation_id,
                kind: EntryKind::new("assistant").expect("entry kind"),
                data: json!("would be dropped"),
                projection: Vec::new(),
                context: ContextControl::none(),
            });
            plan.create_task(PlannedTask {
                conversation_id: ConversationId::new(9_999).expect("conversation id"),
                kind: kind("tool"),
                schema_version: 1,
                input: json!({}),
                dependencies: Vec::new(),
            });
            Ok(TaskCompletion::completed(json!("claimed")).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

fn setup(implementation: Arc<dyn TaskKind>, name: &str) -> (TaskDriver, ion_core::TaskId) {
    let mut session = Session::new().expect("session");
    let task = session
        .create_task(TaskRequest {
            conversation_id: session.root_conversation(),
            kind: kind(name),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .expect("task");
    let mut registry = TaskRegistry::new();
    registry
        .register(kind(name), 1, implementation)
        .expect("register");
    (TaskDriver::new(session, registry), task.task_id)
}

#[tokio::test]
async fn finalization_plan_commits_settlement_entries_and_successors_together() {
    let mut session = Session::new().expect("session");
    let task = session
        .create_task(TaskRequest {
            conversation_id: session.root_conversation(),
            kind: kind("generator"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .expect("task");
    let mut registry = TaskRegistry::new();
    registry
        .register(kind("generator"), 1, Arc::new(Generator))
        .expect("generator");
    registry
        .register(kind("tool"), 1, Arc::new(Plain))
        .expect("tool");
    registry
        .register(kind("join"), 1, Arc::new(Plain))
        .expect("join");
    let driver = TaskDriver::new(session, registry);

    let before = driver.snapshot().await;
    match driver.drive_task(task.task_id).await.expect("drive") {
        DriveOutcome::Settled(settlement) => {
            assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Completed);
        }
        other => panic!("expected settlement, got {other:?}"),
    }

    let after = driver.snapshot().await;
    assert_eq!(after.entries.len(), before.entries.len() + 1);
    assert_eq!(after.tasks.len(), before.tasks.len() + 3);

    let mut successors: Vec<_> = after
        .tasks
        .iter()
        .filter(|record| record.id != task.task_id)
        .collect();
    successors.sort_by_key(|record| record.id.get());
    assert_eq!(successors.len(), 3);
    let tool_ids: Vec<_> = successors
        .iter()
        .filter(|record| record.kind == kind("tool"))
        .map(|record| record.id)
        .collect();
    let join = successors
        .iter()
        .find(|record| record.kind == kind("join"))
        .expect("join task");
    assert_eq!(join.dependencies, tool_ids);
    assert!(
        successors
            .iter()
            .all(|record| matches!(record.status, TaskStatus::Pending))
    );

    // The join only becomes driveable once both tools are terminal; a
    // dependency wait observes that without reserving an invocation.
    let join_id = join.id;
    let mut waiting = Box::pin(driver.wait_dependencies(join_id));
    assert!(futures_pending(&mut waiting));
    drop(waiting);
    assert!(matches!(
        driver
            .snapshot()
            .await
            .tasks
            .iter()
            .find(|r| r.id == join_id)
            .unwrap()
            .status,
        TaskStatus::Pending
    ));
    for tool_id in &tool_ids {
        driver.drive_task(*tool_id).await.expect("drive tool");
    }
    driver.drive_task(join_id).await.expect("drive join");
}

#[tokio::test]
async fn failed_finalization_rolls_back_successors_and_leaves_task_recoverable() {
    let (driver, task_id) = setup(Arc::new(Broken), "broken");
    let before = driver.snapshot().await;

    let error = driver
        .drive_task(task_id)
        .await
        .expect_err("plan must fail");
    assert!(matches!(
        error,
        ion_core::TaskDriverError::Session(ion_core::SessionError::UnknownConversation(_))
    ));

    let after = driver.snapshot().await;
    assert_eq!(after.tasks.len(), before.tasks.len());
    assert_eq!(after.entries.len(), before.entries.len());
    let record = &after.tasks[0];
    assert!(matches!(record.status, TaskStatus::Running));
    assert_eq!(record.generation, 1);
    assert!(!record.cancel_requested);
}
