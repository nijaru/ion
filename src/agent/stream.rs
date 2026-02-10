use crate::agent::AgentEvent;
use crate::agent::context::ContextManager;
use crate::agent::designer::Plan;
use crate::agent::retry::retryable_category;
use crate::compaction::TokenCounter;
use crate::provider::{
    ChatRequest, ContentBlock, LlmApi, Message, StreamEvent, ThinkingConfig, ToolCallEvent,
    ToolDefinition,
};
use crate::session::Session;
use crate::tool::ToolOrchestrator;
use anyhow::Result;
use std::borrow::Cow;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

const MAX_RETRIES: u32 = 3;
/// Cap server-requested retry delays to prevent excessively long waits.
const MAX_RETRY_DELAY: u64 = 60;

/// Context for streaming operations, bundling agent state.
pub(crate) struct StreamContext<'a> {
    pub provider: &'a Arc<dyn LlmApi>,
    pub orchestrator: &'a Arc<ToolOrchestrator>,
    pub context_manager: &'a Arc<ContextManager>,
    pub active_plan: &'a Mutex<Option<Plan>>,
    pub token_counter: &'a TokenCounter,
    pub supports_vision: bool,
}

pub(crate) async fn stream_response(
    ctx: &StreamContext<'_>,
    session: &Session,
    tx: &mpsc::Sender<AgentEvent>,
    thinking: Option<ThinkingConfig>,
    abort_token: CancellationToken,
) -> Result<(Vec<ContentBlock>, Vec<ToolCallEvent>)> {
    let tool_defs: Vec<ToolDefinition> = ctx
        .orchestrator
        .list_tools()
        .into_iter()
        .map(|t| ToolDefinition {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters(),
        })
        .collect();

    let plan = ctx.active_plan.lock().await;
    let assembly = ctx
        .context_manager
        .assemble(&session.messages, None, tool_defs, plan.as_ref())
        .await;

    let messages = if ctx.supports_vision {
        assembly.messages.clone()
    } else {
        strip_images(&assembly.messages)
    };

    let request = ChatRequest {
        model: session.model.clone(),
        messages: Arc::new(messages),
        system: Some(Cow::Owned(assembly.system_prompt.clone())),
        tools: Arc::new(assembly.tools),
        max_tokens: None,
        temperature: None,
        thinking,
    };

    let input_tokens = ctx.token_counter.count_str(&assembly.system_prompt)
        + assembly
            .messages
            .iter()
            .map(|m| ctx.token_counter.count_message(m).total)
            .sum::<usize>();
    let _ = tx.send(AgentEvent::InputTokens(input_tokens)).await;

    let use_streaming = ctx.provider.supports_tool_streaming() || request.tools.is_empty();

    if use_streaming
        && let Some(result) =
            stream_with_retry(ctx.provider, ctx.token_counter, &request, tx, &abort_token).await?
    {
        return Ok(result);
    }
    // Fallback to non-streaming if streaming not supported or returns None

    complete_with_retry(ctx.provider, ctx.token_counter, &request, tx, &abort_token).await
}

/// Attempt streaming completion with retry logic.
/// Returns Some((blocks, calls)) on success, None if fallback to non-streaming is needed.
#[allow(clippy::too_many_lines)]
async fn stream_with_retry(
    provider: &Arc<dyn LlmApi>,
    token_counter: &TokenCounter,
    request: &ChatRequest,
    tx: &mpsc::Sender<AgentEvent>,
    abort_token: &CancellationToken,
) -> Result<Option<(Vec<ContentBlock>, Vec<ToolCallEvent>)>> {
    /// No SSE event received for this duration → treat as stale stream.
    /// Resets on every received event (select! recreates the sleep each iteration).
    const STREAM_STALE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
    let mut retry_count = 0;
    let mut assistant_blocks = Vec::new();
    let mut tool_calls = Vec::new();

    'retry: loop {
        let (stream_tx, mut stream_rx) = mpsc::channel(100);
        let provider = provider.clone();
        let request_clone = request.clone();

        let handle = tokio::spawn(async move { provider.stream(request_clone, stream_tx).await });

        let mut stream_error: Option<String> = None;
        let mut server_retry_after: Option<u64> = None;

        loop {
            tokio::select! {
                () = abort_token.cancelled() => {
                    handle.abort();
                    return Err(anyhow::anyhow!("Cancelled"));
                }
                () = tokio::time::sleep(STREAM_STALE_TIMEOUT) => {
                    warn!("Stream stale: no data received for {}s", STREAM_STALE_TIMEOUT.as_secs());
                    handle.abort();
                    stream_error = Some("Stream timed out (no data received)".to_string());
                    break;
                }
                event = stream_rx.recv() => {
                    match event {
                        Some(StreamEvent::TextDelta(delta)) => {
                            let delta_tokens = token_counter.count_str(&delta);
                            let _ = tx.send(AgentEvent::OutputTokensDelta(delta_tokens)).await;
                            let _ = tx.send(AgentEvent::TextDelta(delta.clone())).await;
                            if let Some(ContentBlock::Text { text }) = assistant_blocks.last_mut() {
                                text.push_str(&delta);
                            } else {
                                assistant_blocks.push(ContentBlock::Text { text: delta });
                            }
                        }
                        Some(StreamEvent::ThinkingDelta(delta)) => {
                            let _ = tx.send(AgentEvent::ThinkingDelta(delta.clone())).await;
                            if let Some(ContentBlock::Thinking { thinking }) = assistant_blocks.last_mut() {
                                thinking.push_str(&delta);
                            } else {
                                assistant_blocks.push(ContentBlock::Thinking { thinking: delta });
                            }
                        }
                        Some(StreamEvent::ToolCall(call)) => {
                            let _ = tx
                                .send(AgentEvent::ToolCallStart(
                                    call.id.clone(),
                                    call.name.clone(),
                                    call.arguments.clone(),
                                ))
                                .await;
                            tool_calls.push(call.clone());
                            assistant_blocks.push(ContentBlock::ToolCall {
                                id: call.id,
                                name: call.name,
                                arguments: call.arguments,
                            });
                        }
                        Some(StreamEvent::Usage(usage)) => {
                            if usage.input_tokens > 0 || usage.output_tokens > 0 {
                                let _ = tx
                                    .send(AgentEvent::ProviderUsage {
                                        input_tokens: usage.input_tokens as usize,
                                        output_tokens: usage.output_tokens as usize,
                                        cache_read_tokens: usage.cache_read_tokens as usize,
                                        cache_write_tokens: usage.cache_write_tokens as usize,
                                    })
                                    .await;
                            }
                        }
                        Some(StreamEvent::Error(e)) => {
                            stream_error = Some(e);
                            break;
                        }
                        Some(StreamEvent::Done) => {}
                        None => {
                            match handle.await {
                                Ok(Err(e)) => {
                                    if let crate::provider::Error::RateLimited { retry_after } = &e {
                                        server_retry_after = *retry_after;
                                    }
                                    stream_error = Some(e.to_string());
                                }
                                Err(join_err) if join_err.is_panic() => {
                                    stream_error = Some("Provider task panicked".to_string());
                                }
                                _ => {}
                            }
                            break;
                        }
                    }
                }
            }
        }

        if let Some(ref err) = stream_error {
            let err_lower = err.to_lowercase();
            let is_tools_not_supported = err_lower.contains("streaming with tools not supported")
                || err_lower.contains("tools not supported")
                || (err_lower.contains("parse") && !request.tools.is_empty());

            if is_tools_not_supported {
                warn!(
                    "Provider doesn't support streaming with tools, falling back to non-streaming"
                );
                return Ok(None); // Signal fallback needed
            } else if let Some(reason) = retryable_category(err)
                && retry_count < MAX_RETRIES
            {
                retry_count += 1;
                let delay = server_retry_after
                    .take()
                    .unwrap_or(1u64 << retry_count)
                    .min(MAX_RETRY_DELAY);
                warn!(
                    "{}, retrying in {}s (attempt {}/{})",
                    reason, delay, retry_count, MAX_RETRIES
                );
                let _ = tx.send(AgentEvent::Retry(reason.to_string(), delay)).await;
                assistant_blocks.clear();
                tool_calls.clear();
                // Interruptible sleep - check abort token during retry delay
                tokio::select! {
                    () = abort_token.cancelled() => {
                        return Err(anyhow::anyhow!("Cancelled"));
                    }
                    () = tokio::time::sleep(std::time::Duration::from_secs(delay)) => {}
                }
                continue 'retry;
            }
            error!("Stream error: {}", err);
            return Err(anyhow::anyhow!("{err}"));
        }
        return Ok(Some((assistant_blocks, tool_calls)));
    }
}

/// Non-streaming completion with retry logic.
async fn complete_with_retry(
    provider: &Arc<dyn LlmApi>,
    token_counter: &TokenCounter,
    request: &ChatRequest,
    tx: &mpsc::Sender<AgentEvent>,
    abort_token: &CancellationToken,
) -> Result<(Vec<ContentBlock>, Vec<ToolCallEvent>)> {
    let mut retry_count = 0u32;

    let completion = loop {
        debug!(
            "Using non-streaming completion (provider: {})",
            provider.id()
        );
        let result = tokio::select! {
            () = abort_token.cancelled() => {
                return Err(anyhow::anyhow!("Cancelled"));
            }
            result = provider.complete(request.clone()) => result
        };

        match result {
            Ok(response) => break response,
            Err(e) => {
                let server_retry_after =
                    if let crate::provider::Error::RateLimited { retry_after } = &e {
                        *retry_after
                    } else {
                        None
                    };
                let err = e.to_string();
                if let Some(reason) = retryable_category(&err)
                    && retry_count < MAX_RETRIES
                {
                    retry_count += 1;
                    let delay = server_retry_after
                        .unwrap_or(1u64 << retry_count)
                        .min(MAX_RETRY_DELAY);
                    warn!(
                        "{}, retrying in {}s (attempt {}/{})",
                        reason, delay, retry_count, MAX_RETRIES
                    );
                    let _ = tx.send(AgentEvent::Retry(reason.to_string(), delay)).await;
                    // Interruptible sleep - check abort token during retry delay
                    tokio::select! {
                        () = abort_token.cancelled() => {
                            return Err(anyhow::anyhow!("Cancelled"));
                        }
                        () = tokio::time::sleep(std::time::Duration::from_secs(delay)) => {}
                    }
                    continue;
                }
                return Err(anyhow::anyhow!("Completion error: {e}"));
            }
        }
    };

    // Emit provider-reported usage
    let usage = &completion.usage;
    if usage.input_tokens > 0 || usage.output_tokens > 0 {
        let _ = tx
            .send(AgentEvent::ProviderUsage {
                input_tokens: usage.input_tokens as usize,
                output_tokens: usage.output_tokens as usize,
                cache_read_tokens: usage.cache_read_tokens as usize,
                cache_write_tokens: usage.cache_write_tokens as usize,
            })
            .await;
    }

    let mut assistant_blocks = Vec::new();
    let mut tool_calls = Vec::new();

    for block in completion.message.content.iter() {
        match block {
            ContentBlock::Text { text } => {
                let tokens = token_counter.count_str(text);
                let _ = tx.send(AgentEvent::OutputTokensDelta(tokens)).await;
                let _ = tx.send(AgentEvent::TextDelta(text.clone())).await;
                assistant_blocks.push(block.clone());
            }
            ContentBlock::Thinking { thinking } => {
                let _ = tx.send(AgentEvent::ThinkingDelta(thinking.clone())).await;
                assistant_blocks.push(block.clone());
            }
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => {
                let _ = tx
                    .send(AgentEvent::ToolCallStart(
                        id.clone(),
                        name.clone(),
                        arguments.clone(),
                    ))
                    .await;
                tool_calls.push(ToolCallEvent {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
                assistant_blocks.push(block.clone());
            }
            _ => {}
        }
    }

    Ok((assistant_blocks, tool_calls))
}

/// Replace Image content blocks with text placeholders for non-vision models.
fn strip_images(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .map(|msg| {
            let has_images = msg
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Image { .. }));
            if !has_images {
                return msg.clone();
            }
            let blocks: Vec<ContentBlock> = msg
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Image { .. } => ContentBlock::Text {
                        text: "[Image attachment (not sent: model does not support vision)]"
                            .to_string(),
                    },
                    other => other.clone(),
                })
                .collect();
            Message {
                role: msg.role,
                content: Arc::new(blocks),
            }
        })
        .collect()
}
