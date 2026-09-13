//! One tool call as an ordinary task kind, plus the catalogue of tools it can
//! reach.
//!
//! A tool receives only its call arguments. It has no session, task or store
//! authority, so it cannot widen what an invocation is allowed to observe.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ion_ai::{ToolCall, ToolResult, ToolSpec};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    AbortContext, ResourceDomain, RunningTask, TaskCompletion, TaskContext, TaskFuture, TaskKind,
    TaskRunError,
};

pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send + 'a>>;

/// One callable tool: a description for the model and an implementation.
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

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

    pub fn register<T: Tool + 'static>(&mut self, tool: T) -> Result<(), ToolCatalogError> {
        let name = tool.spec().name;
        if name.is_empty() {
            return Err(ToolCatalogError::Unnamed);
        }
        if self.tools.insert(name.clone(), Arc::new(tool)).is_some() {
            return Err(ToolCatalogError::Duplicate(name));
        }
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
}

impl TaskKind for ToolKind {
    fn resource_domain(&self) -> Option<ResourceDomain> {
        Some(ResourceDomain::Tool)
    }

    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let call: ToolCall =
                serde_json::from_value(task.input.get("call").cloned().unwrap_or(Value::Null))
                    .map_err(|error| {
                        TaskRunError::new(format!("tool task input is not a tool call: {error}"))
                    })?;

            // An unknown tool is a misconfigured chain rather than a tool-level
            // failure: it is a known application failure, not an interruption.
            let Some(tool) = self.catalog.get(&call.name) else {
                return Ok(TaskCompletion::failed(json!({
                    "call_id": call.id,
                    "name": call.name,
                    "reason": "tool is not registered",
                })));
            };

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
            let result = ToolResult {
                call_id: call.id,
                name: call.name,
                result: value,
            };
            Ok(TaskCompletion::completed(
                serde_json::to_value(result).expect("tool results are serializable"),
            ))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!({"reason": "tool aborted"}))) })
    }
}
