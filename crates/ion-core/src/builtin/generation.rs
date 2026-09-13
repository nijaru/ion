//! Generation as an ordinary task kind.
//!
//! One invocation reads its conversation transcript at a frozen cutoff, projects
//! it into provider-neutral context, adds the input durably bound to this task,
//! calls the model service, and settles with a plan that appends the transcript
//! entries it produced plus the tool children and join the answer requires.

use std::sync::Arc;

use futures_util::StreamExt;
use ion_ai::{
    Content, IncompleteReason, Message, ModelRef, ModelRequest, ModelResponse, ModelService,
    ModelStream, ModelStreamEvent, ProviderError, ResponseTermination, Role, ToolCall, Usage,
};
use serde_json::json;

use super::tool::ToolCatalog;
use super::{ASSISTANT_ENTRY, POST_TOOLS, SCHEMA_VERSION, TOOL, USER_ENTRY, entry_kind, task_kind};
use crate::conversation::context::{ContextControl, project};
use crate::task::{PlannedEntry, PlannedTask, PlannedTaskRef, TaskDependency, TaskPlan};
use crate::{
    AbortContext, Entry, ResourceDomain, RunningTask, TaskCompletion, TaskContext, TaskFuture,
    TaskKind, TaskRunError,
};

/// Transcript page size. History is read in bounded pages rather than as one
/// unbounded clone; total request size stays a P2 bounds decision.
const TRANSCRIPT_PAGE: usize = 64;

pub struct GenerationKind {
    model: ModelRef,
    service: Arc<dyn ModelService>,
    tools: Arc<ToolCatalog>,
}

impl GenerationKind {
    #[must_use]
    pub fn new(model: ModelRef, service: Arc<dyn ModelService>, tools: Arc<ToolCatalog>) -> Self {
        Self {
            model,
            service,
            tools,
        }
    }
}

impl TaskKind for GenerationKind {
    fn resource_domain(&self) -> Option<ResourceDomain> {
        Some(ResourceDomain::Model)
    }

    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let conversation_id = task.conversation_id;

            // Read the transcript once and keep the last entry as the cutoff the
            // request was built against. The projection is the frozen context.
            let entries = read_transcript(&context).await?;
            let cutoff = entries.last().map(|entry| entry.id);
            let projection = project(&entries).map_err(|error| {
                TaskRunError::new(format!(
                    "conversation context is not provider-safe: {error}"
                ))
            })?;

            let mut messages = projection.messages;
            let mut bound_inputs = Vec::new();
            for input in context.assigned_inputs().await? {
                let crate::InputBody::Text(text) = &input.body;
                messages.push(Message {
                    role: Role::User,
                    content: vec![Content::Text(text.clone())],
                    provider_replay: None,
                });
                bound_inputs.push((input.id, text.clone()));
            }

            let request = ModelRequest {
                model: self.model.clone(),
                messages,
                tools: self.tools.specs(),
            };
            let stream = self
                .service
                .stream(request)
                .await
                .map_err(|error| TaskRunError::new(format!("model request failed: {error}")))?;
            let response = tokio::select! {
                response = collect_response(stream) => response
                    .map_err(|error| TaskRunError::new(format!("model stream failed: {error}")))?,
                () = context.cancelled() => {
                    return Err(TaskRunError::new("generation cancelled"));
                }
            };

            // A stream that ended without a complete answer is not a final turn.
            // Nothing is appended and the bound input is not consumed, so the
            // partial answer cannot masquerade as history.
            if !response.is_complete() {
                return Ok(TaskCompletion::failed(json!({
                    "reason": "model response ended before a complete answer",
                    "termination": response.termination,
                    "message": response.message,
                    "usage": response.usage,
                })));
            }

            let calls: Vec<ToolCall> = response
                .message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::ToolCall(call) => Some(call.clone()),
                    Content::Text(_) | Content::ToolResult(_) => None,
                })
                .collect();
            let text = message_text(&response.message);

            let mut plan = TaskPlan::new();
            for (input_id, input_text) in bound_inputs {
                let reference = plan.append_entry(PlannedEntry {
                    conversation_id,
                    kind: entry_kind(USER_ENTRY),
                    data: json!({"text": input_text}),
                    projection: vec![Message {
                        role: Role::User,
                        content: vec![Content::Text(input_text)],
                        provider_replay: None,
                    }],
                    context: ContextControl::none(),
                });
                plan.consume_input(input_id, reference);
            }
            plan.append_entry(PlannedEntry {
                conversation_id,
                kind: entry_kind(ASSISTANT_ENTRY),
                data: json!({
                    "text": text,
                    "tool_calls": calls.len(),
                    "context_cutoff": cutoff.map(|entry| entry.get()),
                    "usage": response.usage,
                    "termination": response.termination,
                }),
                projection: vec![response.message.clone()],
                context: ContextControl::none(),
            });

            if !calls.is_empty() {
                let tools: Vec<PlannedTaskRef> = calls
                    .iter()
                    .map(|call| {
                        plan.create_task(PlannedTask {
                            conversation_id,
                            kind: task_kind(TOOL),
                            schema_version: SCHEMA_VERSION,
                            input: json!({"call": call}),
                            dependencies: Vec::new(),
                            background: false,
                        })
                    })
                    .collect();
                plan.create_task(PlannedTask {
                    conversation_id,
                    kind: task_kind(POST_TOOLS),
                    schema_version: SCHEMA_VERSION,
                    input: json!({}),
                    dependencies: tools.into_iter().map(TaskDependency::Planned).collect(),
                    background: false,
                });
            }

            Ok(TaskCompletion::completed(json!({
                "text": message_text(&response.message),
                "tool_calls": calls.len(),
            }))
            .with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async {
            Ok(TaskCompletion::aborted(
                json!({"reason": "generation aborted"}),
            ))
        })
    }
}

async fn read_transcript(context: &TaskContext) -> Result<Vec<Entry>, TaskRunError> {
    let mut entries = Vec::new();
    let mut after = None;
    loop {
        let page = context.conversation_entries(after, TRANSCRIPT_PAGE).await?;
        after = page.next;
        entries.extend(page.entries);
        if after.is_none() {
            return Ok(entries);
        }
    }
}

/// Fold a stream into a response. Only an explicit `Completed` event is a
/// complete answer; a stream that simply ends is incomplete.
async fn collect_response(mut stream: ModelStream) -> Result<ModelResponse, ProviderError> {
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut usage = Usage::unknown();
    while let Some(event) = stream.next().await {
        match event? {
            ModelStreamEvent::TextDelta(delta) => text.push_str(&delta),
            ModelStreamEvent::ToolCall(call) => calls.push(call),
            ModelStreamEvent::Usage(value) => usage = value,
            ModelStreamEvent::Completed(response) => return Ok(response),
        }
    }
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(Content::Text(text));
    }
    content.extend(calls.into_iter().map(Content::ToolCall));
    Ok(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
            provider_replay: None,
        },
        usage,
        termination: ResponseTermination::Incomplete(IncompleteReason::Other(
            "stream ended without a completed response".to_owned(),
        )),
    })
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::Text(text) => Some(text.as_str()),
            Content::ToolCall(_) | Content::ToolResult(_) => None,
        })
        .collect::<Vec<_>>()
        .join("")
}
