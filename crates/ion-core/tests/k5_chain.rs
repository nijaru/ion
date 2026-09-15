//! K5: a real generation → tools → join → generation chain.
//!
//! The chain runs on readiness dispatch alone once its turn root is driven. It
//! exercises the two read capabilities an invocation needs — its conversation
//! transcript and its dependencies' resolved outcomes — with the tool branches
//! settling out of order.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ion_core::conversation::context::ContextControl;
use ion_core::{
    AbortContext, DriveOutcome, EntryKind, PlannedEntry, PlannedTask, PlannedTurn, RunningTask,
    Session, TaskCompletion, TaskContext, TaskDriver, TaskFuture, TaskId, TaskKind, TaskKindName,
    TaskPlan, TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::json;
use tokio::sync::Notify;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

/// What each handler observed, in the order it observed it.
#[derive(Debug, Default)]
struct Trace(Mutex<Vec<String>>);

impl Trace {
    fn push(&self, value: String) {
        self.0.lock().expect("trace mutex").push(value);
    }

    fn values(&self) -> Vec<String> {
        self.0.lock().expect("trace mutex").clone()
    }
}

#[derive(Clone)]
struct ToolGates {
    /// Held closed until the test has observed the other branch settle.
    release_a: Arc<Notify>,
    gate_a_entered: Arc<Notify>,
    b_settled: Arc<Notify>,
}

/// Reads its conversation and plans the tool branches plus their join.
struct Generation {
    trace: Arc<Trace>,
}

impl TaskKind for Generation {
    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let step = task
                .input
                .get("step")
                .and_then(|value| value.as_str())
                .unwrap_or("first")
                .to_owned();

            // Every invocation reads its transcript through bounded pages inside
            // the boundary it froze first.
            let basis = context.request_basis().await?;
            let mut transcript = Vec::new();
            let mut after = None;
            loop {
                let page = context.request_entries(&basis, after, 1).await?;
                if page.entries.is_empty() {
                    break;
                }
                for entry in &page.entries {
                    transcript.push(entry.kind.as_str().to_owned());
                }
                match page.next {
                    Some(next) => after = Some(next),
                    None => break,
                }
            }
            self.trace.push(format!("generation:{step}:{transcript:?}"));

            if step == "second" {
                return Ok(TaskCompletion::completed(json!({
                    "step": step,
                    "transcript": transcript,
                })));
            }

            let mut plan = TaskPlan::new();
            plan.append_entry(PlannedEntry {
                conversation_id: (task.conversation_id).into(),
                kind: EntryKind::new("assistant").expect("entry kind"),
                data: json!({"text": "calling tools", "calls": ["a", "b"]}),
                projection: Vec::new(),
                context: ContextControl::none(),
            });
            let call = |plan: &mut TaskPlan, name: &str| {
                plan.create_task(PlannedTask {
                    conversation_id: (task.conversation_id).into(),
                    kind: kind("tool"),
                    schema_version: 1,
                    input: json!({"call": name}),
                    dependencies: Vec::new(),
                    turn: PlannedTurn::Inherit,
                })
            };
            let a = call(&mut plan, "a");
            let b = call(&mut plan, "b");
            plan.create_task(PlannedTask {
                conversation_id: (task.conversation_id).into(),
                kind: kind("join"),
                schema_version: 1,
                input: json!({"of": ["a", "b"]}),
                dependencies: vec![
                    ion_core::TaskDependency::Planned(a),
                    ion_core::TaskDependency::Planned(b),
                ],
                turn: PlannedTurn::Inherit,
            });
            Ok(TaskCompletion::completed(json!({"step": step})).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

/// One tool branch. Branch "a" stays in flight until the test releases it, so
/// the branches provably settle out of order.
struct Tool {
    gates: ToolGates,
    trace: Arc<Trace>,
}

impl TaskKind for Tool {
    fn execute<'a>(&'a self, task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let call = task
                .input
                .get("call")
                .and_then(|value| value.as_str())
                .unwrap_or("?")
                .to_owned();
            if call == "a" {
                self.gates.gate_a_entered.notify_one();
                self.gates.release_a.notified().await;
            } else {
                self.gates.b_settled.notify_one();
            }
            self.trace.push(format!("tool:{call}"));
            Ok(TaskCompletion::completed(json!({
                "call": call,
                "result": format!("result-{call}"),
            })))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

/// Joins both branches by reading their resolved outcomes, appends the joined
/// result to the transcript, and starts the continuation generation.
struct Join {
    trace: Arc<Trace>,
}

impl TaskKind for Join {
    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let outcomes = context.dependency_outcomes().await?;
            let joined: Vec<serde_json::Value> = outcomes
                .iter()
                .map(|outcome| {
                    json!({
                        "task": outcome.task_id.get(),
                        "kind": outcome.kind.as_str(),
                        "call": outcome.outcome.value.get("call"),
                        "result": outcome.outcome.value.get("result"),
                    })
                })
                .collect();
            self.trace.push(format!("join:{joined:?}"));

            let mut plan = TaskPlan::new();
            plan.append_entry(PlannedEntry {
                conversation_id: (task.conversation_id).into(),
                kind: EntryKind::new("tool_result").expect("entry kind"),
                data: json!({"results": joined}),
                projection: Vec::new(),
                context: ContextControl::none(),
            });
            plan.create_task(PlannedTask {
                conversation_id: (task.conversation_id).into(),
                kind: kind("generation"),
                schema_version: 1,
                input: json!({"step": "second"}),
                dependencies: Vec::new(),
                turn: PlannedTurn::Inherit,
            });
            Ok(TaskCompletion::completed(json!({"joined": joined})).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

fn registry(gates: ToolGates, trace: Arc<Trace>) -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    registry
        .register(
            kind("generation"),
            1,
            Arc::new(Generation {
                trace: trace.clone(),
            }),
        )
        .expect("generation");
    registry
        .register(
            kind("tool"),
            1,
            Arc::new(Tool {
                gates,
                trace: trace.clone(),
            }),
        )
        .expect("tool");
    registry
        .register(
            kind("join"),
            1,
            Arc::new(Join {
                trace: trace.clone(),
            }),
        )
        .expect("join");
    registry
}

async fn wait_for_terminal(driver: &TaskDriver, task_id: TaskId) -> ion_core::TaskRecord {
    tokio::time::timeout(Duration::from_secs(10), driver.wait_task(task_id))
        .await
        .expect("chain must not stall")
        .expect("wait")
}

#[tokio::test]
async fn generation_tools_join_and_continuation_run_as_one_chain() {
    let mut session = Session::new().expect("session");
    let root_conversation = session.root_conversation();
    let turn = session
        .create_turn(TaskRequest {
            conversation_id: root_conversation,
            kind: kind("generation"),
            schema_version: 1,
            input: json!({"step": "first"}),
            dependencies: Vec::new(),
        })
        .expect("turn");

    let gates = ToolGates {
        release_a: Arc::new(Notify::new()),
        gate_a_entered: Arc::new(Notify::new()),
        b_settled: Arc::new(Notify::new()),
    };
    let trace = Arc::new(Trace::default());
    let driver = TaskDriver::new(session, registry(gates.clone(), trace.clone()));

    let outcome = driver.drive_task(turn.task_id).await.expect("drive root");
    assert!(matches!(outcome, DriveOutcome::Settled(_)));

    // Both tool branches are dispatched by readiness alone. "a" is in flight,
    // so "b" settles first.
    gates.gate_a_entered.notified().await;
    tokio::time::timeout(Duration::from_secs(10), gates.b_settled.notified())
        .await
        .expect("the other branch must settle while the first is still in flight");

    let snapshot = driver.snapshot().await;
    let branch = |call: &str| {
        snapshot
            .tasks
            .iter()
            .find(|record| record.input.get("call").and_then(|v| v.as_str()) == Some(call))
            .expect("tool branch")
    };
    assert!(matches!(branch("b").status, TaskStatus::Terminal(_)));
    assert!(matches!(branch("a").status, TaskStatus::Running));
    let join = snapshot
        .tasks
        .iter()
        .find(|record| record.kind == kind("join"))
        .expect("join task");
    assert!(matches!(join.status, TaskStatus::Pending));
    // The whole chain still occupies the turn's foreground slot.
    assert_eq!(
        snapshot
            .conversations
            .iter()
            .find(|conversation| conversation.id == root_conversation)
            .expect("conversation")
            .foreground_turn,
        Some(turn.task_id)
    );

    gates.release_a.notify_one();

    // The join runs when both branches are terminal, and its continuation
    // generation runs after it.
    let join_record = wait_for_terminal(&driver, join.id).await;
    let TaskStatus::Terminal(join_outcome) = &join_record.status else {
        panic!("join must be terminal");
    };
    assert_eq!(join_outcome.kind, ion_core::TaskOutcomeKind::Completed);
    let continuation = driver
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|record| record.input.get("step").and_then(|v| v.as_str()) == Some("second"))
        .map(|record| record.id)
        .expect("continuation generation");
    let continuation_record = wait_for_terminal(&driver, continuation).await;

    // The join saw both branches in dependency order even though "b" settled
    // first, and it saw their resolved values rather than placeholders.
    let joined = join_outcome.value["joined"].clone();
    let calls: Vec<_> = joined
        .as_array()
        .expect("joined results")
        .iter()
        .map(|entry| entry["call"].as_str().expect("call").to_owned())
        .collect();
    assert_eq!(calls, vec!["a", "b"]);
    assert_eq!(joined[0]["result"], json!("result-a"));
    assert_eq!(joined[1]["result"], json!("result-b"));

    // The continuation read the transcript the join appended to.
    let TaskStatus::Terminal(continuation_outcome) = &continuation_record.status else {
        panic!("continuation must be terminal");
    };
    let transcript: Vec<String> = continuation_outcome.value["transcript"]
        .as_array()
        .expect("transcript")
        .iter()
        .map(|value| value.as_str().expect("entry kind").to_owned())
        .collect();
    assert!(transcript.contains(&"assistant".to_owned()));
    assert!(transcript.contains(&"tool_result".to_owned()));

    // Out-of-order settlement is visible, and the chain completed the turn.
    assert_eq!(trace.values()[0], "generation:first:[]");
    assert!(trace.values().iter().any(|value| value == "tool:b"));
    assert!(trace.values().iter().any(|value| value == "tool:a"));
    assert!(
        trace
            .values()
            .iter()
            .any(|value| value.starts_with("join:"))
    );
    let snapshot = driver.snapshot().await;
    assert_eq!(
        snapshot
            .conversations
            .iter()
            .find(|conversation| conversation.id == root_conversation)
            .expect("conversation")
            .foreground_turn,
        None,
        "the completed chain releases the slot"
    );
    assert!(
        snapshot
            .tasks
            .iter()
            .all(|record| matches!(record.status, TaskStatus::Terminal(_)))
    );

    // The continuation saw the result entry, so the turn really did continue
    // rather than start from an empty transcript.
    assert!(
        trace
            .values()
            .iter()
            .any(|value| value.contains("generation:second:") && value.contains("tool_result"))
    );
}
