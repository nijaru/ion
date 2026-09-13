//! The post-tools join: the barrier between finished tool tasks and the next
//! generation.
//!
//! Each tool task records its own result entry when it settles, so transcript
//! chronology records completion order while model projection restores the
//! originating call order. The join therefore appends nothing; it waits until
//! every call has a recorded result and only then makes the continuation
//! generation runnable, which is what keeps an incomplete exchange from
//! becoming model context.

use serde_json::json;

use super::{GENERATION, SCHEMA_VERSION, task_kind};
use crate::task::{PlannedTask, PlannedTurn, TaskPlan};
use crate::{
    AbortContext, RunningTask, TaskCompletion, TaskContext, TaskFuture, TaskKind, TaskOutcomeKind,
    TaskRunError,
};

pub struct PostToolsKind;

impl TaskKind for PostToolsKind {
    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let outcomes = context.dependency_outcomes().await?;
            // Every tool task settles a recorded result entry on success, tool
            // error, cancellation and unknown outcome. A dependency that settled
            // `Failed` or `Unsupported` could not record one, so starting another
            // generation would build context from a split exchange.
            for outcome in &outcomes {
                if !matches!(
                    outcome.outcome.kind,
                    TaskOutcomeKind::Completed
                        | TaskOutcomeKind::Aborted
                        | TaskOutcomeKind::Indeterminate
                ) {
                    return Err(TaskRunError::new(format!(
                        "tool task {} settled {:?} without recording a result",
                        outcome.task_id, outcome.outcome.kind
                    )));
                }
            }

            let mut plan = TaskPlan::new();
            plan.create_task(PlannedTask {
                conversation_id: (task.conversation_id).into(),
                kind: task_kind(GENERATION),
                schema_version: SCHEMA_VERSION,
                input: json!({}),
                dependencies: Vec::new(),
                turn: PlannedTurn::Inherit,
            });

            Ok(TaskCompletion::completed(json!({"tool_results": outcomes.len()})).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async {
            Ok(TaskCompletion::aborted(
                json!({"reason": "post-tools join aborted"}),
            ))
        })
    }
}
