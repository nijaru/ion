//! Generation as an ordinary task kind.
//!
//! One invocation reads its conversation transcript at a frozen cutoff, projects
//! it into provider-neutral context (which already carries any accepted input the
//! session placed there), and freezes the complete model request in its durable
//! checkpoint *before* dispatch. Recovery therefore replays the recorded request
//! instead of silently rebuilding a different one from changed history, inputs or
//! configuration, and every attempt is accounted for.
//!
//! The settlement appends the transcript entries the answer produced and creates
//! the tool children plus the join. It places nothing: placement happened when
//! the input was bound to this turn, so a failed answer cannot strand an accepted
//! message outside history.

use std::sync::Arc;

use futures_util::StreamExt;
use ion_ai::{
    Content, IncompleteReason, Message, ModelRef, ModelRequest, ModelResponse, ModelService,
    ModelStream, ModelStreamEvent, ProviderError, ResponseTermination, Role, ToolCall, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::checkpoint::{Checkpoint, decode};
use super::tool::ToolCatalog;
use super::{ASSISTANT_ENTRY, POST_TOOLS, SCHEMA_VERSION, TOOL, entry_kind, task_kind};
use crate::conversation::context::{ContextControl, project};
use crate::task::{
    PlannedEntry, PlannedTask, PlannedTaskRef, PlannedTurn, TaskDependency, TaskPlan,
};
use crate::{
    AbortContext, Entry, EntryId, InputId, ResourceDomain, RunningTask, TaskCompletion,
    TaskContext, TaskFuture, TaskKind, TaskOutcomeKind, TaskRunError,
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

    /// Build the request this invocation will send, freezing the transcript
    /// cutoff, the bound inputs and the tool specifications with it.
    async fn freeze(&self, context: &TaskContext) -> Result<FrozenRequest, TaskRunError> {
        let entries = read_transcript(context).await?;
        let context_cutoff = entries.last().map(|entry| entry.id);
        let projection = project(&entries).map_err(|error| {
            TaskRunError::new(format!(
                "conversation context is not provider-safe: {error}"
            ))
        })?;

        // Accepted input is already in the transcript: the writer placed it when
        // it bound the input to this turn, so the projection above carries it and
        // this invocation adds nothing. What it records is which accepted inputs
        // this request actually included, taken from the projection that built the
        // request: presence in the transcript, or an id below the cutoff, does not
        // establish that an edit did not remove the content.
        let inputs = context
            .placed_inputs()
            .await?
            .into_iter()
            .filter(|input| {
                input
                    .disposition
                    .placement()
                    .is_some_and(|placement| projection.contributing.contains(&placement.entry))
            })
            .map(|input| input.id)
            .collect();

        Ok(FrozenRequest {
            request: ModelRequest {
                model: self.model.clone(),
                messages: projection.messages,
                tools: self.tools.specs(),
            },
            context_cutoff,
            inputs,
            attempts: 0,
        })
    }
}

impl TaskKind for GenerationKind {
    fn resource_domain(&self) -> Option<ResourceDomain> {
        Some(ResourceDomain::Model)
    }

    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let conversation_id = task.conversation_id;
            // Recovery reuses the recorded request verbatim; only a first attempt
            // derives one from current history and configuration. A request that
            // was recorded but cannot be read is not a first attempt, so it is
            // neither rebuilt from changed state nor dispatched again.
            let mut frozen = match decode::<FrozenRequest>(task.checkpoint.as_ref()) {
                Checkpoint::Valid(frozen) => frozen,
                Checkpoint::Absent => self.freeze(&context).await?,
                Checkpoint::Unreadable => {
                    return Ok(TaskCompletion::terminal(
                        TaskOutcomeKind::Indeterminate,
                        json!({
                            "reason": "the recorded model request is unreadable; \
                                       the attempt is neither rebuilt nor repeated",
                            "recorded_request": "unreadable",
                        }),
                    ));
                }
            };
            frozen.attempts += 1;
            context.checkpoint(Some(encode(&frozen)?), None).await?;

            // Cancellation owns opening as well as collection. The checkpoint
            // remains dispatch evidence: dropping local ownership does not prove
            // the provider did not receive or bill this attempt.
            let response = tokio::select! {
                biased;
                () = context.cancelled() => {
                    return Err(TaskRunError::new("generation cancelled"));
                }
                response = async {
                    let stream = self.service.stream(frozen.request.clone()).await
                        .map_err(|error| TaskRunError::new(format!("model request failed: {error}")))?;
                    collect_response(stream).await
                        .map_err(|error| TaskRunError::new(format!("model stream failed: {error}")))
                } => response?,
            };

            // A stream that ended without a complete answer is not a final turn.
            // Nothing is appended and the bound inputs are not consumed, so the
            // partial answer cannot masquerade as history.
            if !response.is_complete() {
                return Ok(TaskCompletion::failed(json!({
                    "reason": "model response ended before a complete answer",
                    "termination": response.termination,
                    "message": response.message,
                    "usage": response.usage,
                    "attempts": frozen.attempts,
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

            // The accepted input is already placed, so the settlement appends only
            // what the answer produced. A cancelled or failed answer therefore
            // leaves the accepted message in the history.
            let mut plan = TaskPlan::new();
            plan.append_entry(PlannedEntry {
                conversation_id: conversation_id.into(),
                kind: entry_kind(ASSISTANT_ENTRY),
                data: json!({
                    "text": text,
                    "tool_calls": calls.len(),
                    "context_cutoff": frozen.context_cutoff.map(|entry| entry.get()),
                    "attempts": frozen.attempts,
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
                            conversation_id: conversation_id.into(),
                            kind: task_kind(TOOL),
                            schema_version: SCHEMA_VERSION,
                            input: json!({"call": call}),
                            dependencies: Vec::new(),
                            turn: PlannedTurn::Inherit,
                        })
                    })
                    .collect();
                plan.create_task(PlannedTask {
                    conversation_id: conversation_id.into(),
                    kind: task_kind(POST_TOOLS),
                    schema_version: SCHEMA_VERSION,
                    input: json!({}),
                    dependencies: tools.into_iter().map(TaskDependency::Planned).collect(),
                    turn: PlannedTurn::Inherit,
                });
            }

            Ok(TaskCompletion::completed(json!({
                "text": message_text(&response.message),
                "tool_calls": calls.len(),
                "attempts": frozen.attempts,
            }))
            .with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, task: RunningTask, _context: AbortContext) -> TaskFuture<'a> {
        Box::pin(async move {
            // Dropping an invocation never recalls a request the provider may
            // already have received and billed, so abort keeps the prepared
            // attempt count rather than reporting the attempt as never sent.
            // Unreadable attempt evidence is not an absence of dispatch, and it
            // is not a known application failure either.
            match decode::<FrozenRequest>(task.checkpoint.as_ref()) {
                Checkpoint::Absent => Ok(TaskCompletion::aborted(json!({
                    "reason": "generation aborted before dispatch",
                    "attempts": 0,
                }))),
                Checkpoint::Valid(frozen) => Ok(TaskCompletion::aborted(json!({
                    "reason": "generation aborted",
                    "attempts": frozen.attempts,
                }))),
                Checkpoint::Unreadable => Ok(TaskCompletion::terminal(
                    TaskOutcomeKind::Indeterminate,
                    json!({
                        "reason": "the recorded attempt evidence is unreadable; \
                                   the provider may have received this attempt",
                        "attempts": "unreadable",
                    }),
                )),
            }
        })
    }
}

/// The complete, durable request this invocation dispatches.
///
/// It is the task checkpoint, so a replacement invocation cannot silently
/// continue against a different transcript, input set, model or tool catalogue.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FrozenRequest {
    request: ModelRequest,
    context_cutoff: Option<EntryId>,
    /// The accepted inputs whose placed entries this request's transcript
    /// included, for provenance only. The entry is the content authority, so the
    /// text is not duplicated here.
    inputs: Vec<InputId>,
    attempts: u32,
}

fn encode(frozen: &FrozenRequest) -> Result<Value, TaskRunError> {
    serde_json::to_value(frozen)
        .map_err(|error| TaskRunError::new(format!("frozen request is not encodable: {error}")))
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
