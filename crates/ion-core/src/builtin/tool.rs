//! One tool call as an ordinary task kind, plus the catalogue of tools it can
//! reach.
//!
//! A tool receives only its call arguments. It has no session, task or store
//! authority, so it cannot widen what an invocation is allowed to observe.
//!
//! The task owns its own result entry: whichever way the call ends — a result, a
//! request error, or an unreconcilable cancellation — exactly one `tool_result`
//! entry is committed with the terminal outcome. That is what lets a cancelled
//! chain still leave a complete, provider-safe transcript.
//!
//! A call that may reach the outside world records a durable dispatch before it
//! is attempted. Recovery therefore distinguishes "never dispatched" (safe to
//! run) from "dispatched with an unknown outcome" (never repeated silently, and
//! never reported as stopped).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ion_ai::{Content, Message, Role, ToolCall, ToolResult, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use super::{TOOL_RESULT_ENTRY, entry_kind};
use crate::conversation::context::ContextControl;
use crate::task::{PlannedEntry, TaskPlan};
use crate::{
    AbortContext, ConversationId, ResourceDomain, RunningTask, TaskCompletion, TaskContext,
    TaskFuture, TaskKind, TaskOutcomeKind, TaskRunError,
};

pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send + 'a>>;

/// One callable tool: a description for the model and an implementation.
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    /// Whether repeating this call is safe when a previous attempt's outcome is
    /// unknown.
    ///
    /// The default is `false`: a tool that can change external state must say so
    /// explicitly. A non-retry-safe call whose dispatch was recorded but never
    /// settled by an unknown outcome instead of being repeated or reported as
    /// stopped.
    fn retry_safe(&self) -> bool {
        false
    }

    /// Run one call. A tool-level failure is reported to the model as the call
    /// result, which is why it is a value rather than a task failure.
    fn call<'a>(&'a self, arguments: Value) -> ToolFuture<'a>;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct ToolError(String);

impl ToolError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The tools one built-in chain may call. Registration is by the spec name.
#[derive(Default)]
pub struct ToolCatalog {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolCatalog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A rejected registration leaves the catalogue unchanged.
    pub fn register<T: Tool + 'static>(&mut self, tool: T) -> Result<(), ToolCatalogError> {
        let name = tool.spec().name;
        if name.is_empty() {
            return Err(ToolCatalogError::Unnamed);
        }
        if self.tools.contains_key(&name) {
            return Err(ToolCatalogError::Duplicate(name));
        }
        self.tools.insert(name, Arc::new(tool));
        Ok(())
    }

    /// Tool specifications in a stable order, so a request is reproducible.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<_> = self.tools.values().map(|tool| tool.spec()).collect();
        specs.sort_by(|left, right| left.name.cmp(&right.name));
        specs
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolCatalogError {
    #[error("tool name cannot be empty")]
    Unnamed,
    #[error("tool {0} is already registered")]
    Duplicate(String),
}

/// Executes exactly one tool call from its input and settles with the result.
pub struct ToolKind {
    catalog: Arc<ToolCatalog>,
}

impl ToolKind {
    #[must_use]
    pub fn new(catalog: Arc<ToolCatalog>) -> Self {
        Self { catalog }
    }

    fn dispatch<'a>(
        &'a self,
        task: RunningTask,
        context: TaskContext,
        recovering: bool,
    ) -> TaskFuture<'a> {
        Box::pin(async move {
            let call = parse_call(&task.input)?;
            let previous = dispatch_of(task.checkpoint.as_ref());

            // No external action is possible, so the truthful outcome is a
            // request error the model can read and correct.
            let Some(tool) = self.catalog.get(&call.name) else {
                return Ok(recorded(
                    task.conversation_id,
                    call,
                    json!({"error": "tool is not registered"}),
                    TaskOutcomeKind::Completed,
                    "not_registered",
                ));
            };

            // A recorded dispatch whose invocation disappeared has an unknown
            // external outcome. Repeating a non-retry-safe call could duplicate
            // the action, so retain the uncertainty instead.
            if recovering && previous.is_some() && !tool.retry_safe() {
                return Ok(recorded(
                    task.conversation_id,
                    call,
                    json!({"error": "the previous attempt was dispatched; its outcome is unknown"}),
                    TaskOutcomeKind::Indeterminate,
                    "unreconciled",
                ));
            }

            let attempts = previous.as_ref().map_or(0, |dispatch| dispatch.attempts);
            context
                .checkpoint(
                    Some(json!({
                        "dispatch": Dispatch {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            attempts: attempts + 1,
                        }
                    })),
                    None,
                )
                .await?;

            let result = tokio::select! {
                result = tool.call(call.arguments.clone()) => result,
                () = context.cancelled() => {
                    return Err(TaskRunError::new("tool call cancelled"));
                }
            };
            let value = match result {
                Ok(value) => value,
                Err(error) => json!({"error": error.to_string()}),
            };
            Ok(recorded(
                task.conversation_id,
                call,
                value,
                TaskOutcomeKind::Completed,
                "completed",
            ))
        })
    }
}

impl TaskKind for ToolKind {
    fn resource_domain(&self) -> Option<ResourceDomain> {
        Some(ResourceDomain::Tool)
    }

    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.dispatch(task, context, false)
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.dispatch(task, context, true)
    }

    fn abort<'a>(&'a self, task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let call = parse_call(&task.input)?;
            let dispatch = dispatch_of(task.checkpoint.as_ref());
            let retry_safe = self
                .catalog
                .get(&call.name)
                .is_some_and(|tool| tool.retry_safe());

            // A dropped invocation does not cancel an external call that was
            // already handed over, so claiming it stopped would be a guess.
            if dispatch.is_some() && !retry_safe {
                return Ok(recorded(
                    task.conversation_id,
                    call,
                    json!({"error": "cancelled; the dispatched call's outcome is unknown"}),
                    TaskOutcomeKind::Indeterminate,
                    "indeterminate",
                ));
            }
            let message = if dispatch.is_some() {
                "cancelled"
            } else {
                "cancelled before dispatch"
            };
            Ok(recorded(
                task.conversation_id,
                call,
                json!({"error": message}),
                TaskOutcomeKind::Aborted,
                "aborted",
            ))
        })
    }
}

/// Durable evidence that a call was handed to a tool, written before the call
/// so a replacement invocation can tell "never dispatched" from "outcome
/// unknown".
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Dispatch {
    call_id: String,
    name: String,
    attempts: u32,
}

fn dispatch_of(checkpoint: Option<&Value>) -> Option<Dispatch> {
    serde_json::from_value(checkpoint?.get("dispatch")?.clone()).ok()
}

fn parse_call(input: &Value) -> Result<ToolCall, TaskRunError> {
    serde_json::from_value(input.get("call").cloned().unwrap_or(Value::Null))
        .map_err(|error| TaskRunError::new(format!("tool task input is not a tool call: {error}")))
}

/// Settle with the tool result and the single entry that records it.
fn recorded(
    conversation_id: ConversationId,
    call: ToolCall,
    value: Value,
    kind: TaskOutcomeKind,
    status: &str,
) -> TaskCompletion {
    let result = ToolResult {
        call_id: call.id,
        name: call.name,
        result: value,
    };
    let mut plan = TaskPlan::new();
    plan.append_entry(PlannedEntry {
        conversation_id,
        kind: entry_kind(TOOL_RESULT_ENTRY),
        data: json!({
            "call_id": result.call_id,
            "name": result.name,
            "status": status,
        }),
        projection: vec![Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(result.clone())],
            provider_replay: None,
        }],
        context: ContextControl::none(),
    });
    TaskCompletion::terminal(
        kind,
        serde_json::to_value(result).expect("tool results are serializable"),
    )
    .with_plan(plan)
}
