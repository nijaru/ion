//! The post-tools join: turn finished tool tasks into one transcript entry and
//! start the next generation.
//!
//! Tool tasks settle in completion order, but a provider request requires tool
//! results in the order of the assistant call that produced them. The join reads
//! its dependencies in dependency order, which is call order, and appends them
//! as consecutive tool messages.

use ion_ai::{Content, Message, Role, ToolResult};
use serde_json::json;

use super::{GENERATION, SCHEMA_VERSION, TOOL_RESULT_ENTRY, entry_kind, task_kind};
use crate::conversation::context::ContextControl;
use crate::task::{PlannedEntry, PlannedTask, TaskPlan};
use crate::{
    AbortContext, RunningTask, TaskCompletion, TaskContext, TaskFuture, TaskKind, TaskOutcomeKind,
    TaskRunError,
};

pub struct PostToolsKind;

impl TaskKind for PostToolsKind {
    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let outcomes = context.dependency_outcomes().await?;
            let mut results = Vec::with_capacity(outcomes.len());
            for outcome in &outcomes {
                // A tool that did not complete leaves the exchange incomplete.
                // Staying recoverable is safer than presenting a partial or
                // fabricated result to the model as the call's outcome.
                if outcome.outcome.kind != TaskOutcomeKind::Completed {
                    return Err(TaskRunError::new(format!(
                        "tool task {} settled {:?}",
                        outcome.task_id, outcome.outcome.kind
                    )));
                }
                let result: ToolResult = serde_json::from_value(outcome.outcome.value.clone())
                    .map_err(|error| {
                        TaskRunError::new(format!(
                            "tool task {} produced an invalid result: {error}",
                            outcome.task_id
                        ))
                    })?;
                results.push(result);
            }

            let projection: Vec<Message> = results
                .iter()
                .cloned()
                .map(|result| Message {
                    role: Role::Tool,
                    content: vec![Content::ToolResult(result)],
                    provider_replay: None,
                })
                .collect();

            let mut plan = TaskPlan::new();
            plan.append_entry(PlannedEntry {
                conversation_id: task.conversation_id,
                kind: entry_kind(TOOL_RESULT_ENTRY),
                data: json!({"count": results.len()}),
                projection,
                context: ContextControl::none(),
            });
            plan.create_task(PlannedTask {
                conversation_id: task.conversation_id,
                kind: task_kind(GENERATION),
                schema_version: SCHEMA_VERSION,
                input: json!({}),
                dependencies: Vec::new(),
                background: false,
            });

            Ok(TaskCompletion::completed(json!({"results": results.len()})).with_plan(plan))
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
