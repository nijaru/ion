use std::sync::Arc;

use ion_core::conversation::context::ContextControl;
use ion_core::{
    EntryKind, PlannedEntry, TaskCompletion, TaskContext, TaskDriver, TaskKindName,
    TaskOutcomeKind, TaskPlan, TaskRegistry, TaskRequest, TypedAbortContext, TypedContext,
    TypedFuture, TypedHandler, TypedReport, TypedTask,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Progress {
    step: u32,
}

/// Typed handler that exercises decode, checkpoint, typed completion, a
/// finalization plan, and a typed failure/abort path.
struct Typed {
    fail: bool,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Input {
    label: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Failed {
    reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Aborted {
    note: String,
}

impl TypedHandler for Typed {
    type Input = Input;
    type Checkpoint = Progress;
    type Completed = String;
    type Failure = Failed;
    type Aborted = Aborted;

    fn execute<'a>(
        &'a self,
        task: TypedTask<Self>,
        context: TypedContext<Self>,
    ) -> TypedFuture<'a, Self> {
        let fail = self.fail;
        Box::pin(async move {
            let step = task.checkpoint.map_or(0, |progress| progress.step);
            context
                .checkpoint(Some(Progress { step: step + 1 }), None)
                .await?;
            if fail {
                return Ok(TypedReport::failed(Failed {
                    reason: "typed failure".to_owned(),
                }));
            }
            let mut plan = TaskPlan::new();
            plan.append_entry(PlannedEntry {
                conversation_id: task.conversation_id,
                kind: EntryKind::new("assistant").expect("entry kind"),
                data: json!(task.input.label),
                projection: Vec::new(),
                context: ContextControl::none(),
            });
            Ok(
                TypedReport::completed(format!("{}#{}", task.input.label, step + 1))
                    .with_plan(plan),
            )
        })
    }

    fn recover<'a>(
        &'a self,
        task: TypedTask<Self>,
        context: TypedContext<Self>,
    ) -> TypedFuture<'a, Self> {
        self.execute(task, context)
    }

    fn abort<'a>(
        &'a self,
        _task: TypedTask<Self>,
        context: TypedAbortContext<Self>,
    ) -> TypedFuture<'a, Self> {
        Box::pin(async move {
            context.checkpoint(Some(Progress { step: 0 }), None).await?;
            Ok(TypedReport::aborted(Aborted {
                note: "typed abort".to_owned(),
            }))
        })
    }
}

fn setup(fail: bool) -> (TaskDriver, ion_core::TaskId) {
    let mut session = ion_core::Session::new().expect("session");
    let kind = TaskKindName::new("typed").expect("kind");
    let task = session
        .create_task(TaskRequest {
            conversation_id: session.root_conversation(),
            kind: kind.clone(),
            schema_version: 1,
            input: json!({"label": "alpha"}),
            dependencies: Vec::new(),
        })
        .expect("task");
    let mut registry = TaskRegistry::new();
    registry
        .register_typed(kind, 1, Arc::new(Typed { fail }))
        .expect("register typed");
    (TaskDriver::new(session, registry), task.task_id)
}

fn settled(outcome: ion_core::DriveOutcome) -> ion_core::Settlement {
    match outcome {
        ion_core::DriveOutcome::Settled(settlement) => settlement,
        other => panic!("expected settlement, got {other:?}"),
    }
}

#[tokio::test]
async fn typed_handler_decodes_input_encodes_completion_and_applies_plan() {
    let (driver, task_id) = setup(false);
    let before = driver.snapshot().await;

    let outcome = settled(driver.drive_task(task_id).await.expect("drive"));
    assert_eq!(outcome.outcome.kind, TaskOutcomeKind::Completed);
    assert_eq!(outcome.outcome.value, json!("alpha#1"));

    let after = driver.snapshot().await;
    let record = after.tasks.iter().find(|r| r.id == task_id).expect("task");
    // The typed checkpoint round-tripped through its durable JSON shape.
    assert_eq!(record.checkpoint, Some(json!({"step": 1})));
    assert_eq!(after.entries.len(), before.entries.len() + 1);
    assert_eq!(after.entries[0].data, json!("alpha"));
}

#[tokio::test]
async fn typed_handler_maps_failure_and_abort_to_distinct_outcomes() {
    let (driver, task_id) = setup(true);
    let outcome = settled(driver.drive_task(task_id).await.expect("drive"));
    assert_eq!(outcome.outcome.kind, TaskOutcomeKind::Failed);
    assert_eq!(outcome.outcome.value, json!({"reason": "typed failure"}));

    let (driver, task_id) = setup(false);
    driver.cancel_task(task_id).await.expect("cancel");
    let outcome = settled(driver.drive_task(task_id).await.expect("drive abort"));
    assert_eq!(outcome.outcome.kind, TaskOutcomeKind::Aborted);
    assert_eq!(outcome.outcome.value, json!({"note": "typed abort"}));
}

#[tokio::test]
async fn undecodable_durable_input_settles_structured_failure() {
    let mut session = ion_core::Session::new().expect("session");
    let kind = TaskKindName::new("typed").expect("kind");
    let task = session
        .create_task(TaskRequest {
            conversation_id: session.root_conversation(),
            kind: kind.clone(),
            schema_version: 1,
            // `label` is a number, which the typed `Input` cannot decode.
            input: json!({"label": 7}),
            dependencies: Vec::new(),
        })
        .expect("task");
    let mut registry = TaskRegistry::new();
    registry
        .register_typed(kind, 1, Arc::new(Typed { fail: false }))
        .expect("register typed");
    let driver = TaskDriver::new(session, registry);

    let outcome = settled(driver.drive_task(task.task_id).await.expect("drive"));
    assert_eq!(outcome.outcome.kind, TaskOutcomeKind::Failed);
    assert_eq!(outcome.outcome.value["code"], json!("decode_failed"));
    assert_eq!(outcome.outcome.value["field"], json!("input"));
}

#[tokio::test]
async fn typed_registration_rejects_duplicate_kind_and_revision() {
    let mut registry = TaskRegistry::new();
    let kind = TaskKindName::new("typed").expect("kind");
    registry
        .register_typed(kind.clone(), 1, Arc::new(Typed { fail: false }))
        .expect("first");
    let duplicate = registry
        .register_typed(kind, 1, Arc::new(Typed { fail: false }))
        .expect_err("duplicate");
    assert!(matches!(
        duplicate,
        ion_core::TaskRegistryError::Duplicate { .. }
    ));
}

#[allow(dead_code)]
fn erased_registry_accepts_plain_kinds() {
    // The typed adapter shares one registry with erased `TaskKind`s.
    let mut registry = TaskRegistry::new();
    registry
        .register(
            TaskKindName::new("plain").expect("kind"),
            1,
            Arc::new(PlainKind),
        )
        .expect("register plain");
}

struct PlainKind;

impl ion_core::TaskKind for PlainKind {
    fn execute<'a>(
        &'a self,
        _task: ion_core::RunningTask,
        _context: TaskContext,
    ) -> ion_core::TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::completed(json!("plain"))) })
    }

    fn recover<'a>(
        &'a self,
        task: ion_core::RunningTask,
        context: TaskContext,
    ) -> ion_core::TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(
        &'a self,
        _task: ion_core::RunningTask,
        _context: ion_core::AbortContext,
    ) -> ion_core::TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("plain"))) })
    }
}
