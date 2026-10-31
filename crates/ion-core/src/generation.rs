//! One provider request lifecycle: streaming, cancellation, and bounded pre-output retry.
use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use ion_ai::{
    Content, IncompleteReason, ModelExecution, ModelRequest, ModelResponse, ModelRoute,
    ModelRouteReason, ModelService, ModelStreamEvent, ProviderError, ProviderErrorKind,
    ResponseTermination, Usage,
};
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentError, AgentEvent};

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(crate) struct GeneratedResponse {
    pub response: ModelResponse,
    pub route: ModelRoute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputCustody {
    Unobserved,
    Observed,
}

/// Evidence from the service boundary, independent of observer delivery. The
/// final attempt retains its actual route and clock even when generation fails.
#[derive(Debug)]
pub(crate) struct GenerationAttempt {
    pub outcome: Result<ModelResponse, AgentError>,
    pub route: ModelRoute,
    pub output: OutputCustody,
    pub timing: std::ops::Range<tokio::time::Instant>,
}

impl GenerationAttempt {
    pub fn context_length_exceeded(&self) -> bool {
        matches!(
            &self.outcome,
            Err(AgentError::Provider(ProviderError {
                kind: ProviderErrorKind::ContextLength,
                ..
            })) | Ok(ModelResponse {
                termination: ResponseTermination::Incomplete(IncompleteReason::ContextLength),
                ..
            })
        )
    }

    pub fn into_result(self) -> Result<GeneratedResponse, AgentError> {
        self.outcome.map(|response| GeneratedResponse {
            response,
            route: self.route,
        })
    }
}

pub(crate) async fn generate_with_retry<F>(
    model: &Arc<dyn ModelService>,
    request: &ModelRequest,
    stop: &CancellationToken,
    observe: &mut F,
) -> GenerationAttempt
where
    F: FnMut(AgentEvent) + Send,
{
    if let Err(error) = model.validate_controls(&request.route.effective, &request.controls) {
        let now = tokio::time::Instant::now();
        return GenerationAttempt {
            outcome: Err(error.into()),
            route: request.route.clone(),
            output: OutputCustody::Unobserved,
            timing: now..now,
        };
    }
    for attempt in 0..=2 {
        // The caller retains the frozen base; only the dispatched attempt
        // needs an owned copy. Borrowing avoids a second full-history clone.
        let mut attempt_request = request.clone();
        if attempt > 0 {
            attempt_request.route.reason = ModelRouteReason::Retry;
        }
        let mut evidence = generate_once(model, attempt_request, stop, observe).await;
        match &evidence.outcome {
            Err(AgentError::Provider(error))
                if evidence.output == OutputCustody::Unobserved
                    && attempt < 2
                    && retryable_provider_error(error) =>
            {
                // A server's pacing takes precedence over local backoff.
                // Leave long waits to the caller instead of holding a Turn
                // open for an unbounded provider-requested interval.
                let delay_ms = error.retry_after_ms.unwrap_or(500 * (1 << attempt));
                if delay_ms > 60_000 {
                    return evidence;
                }
                let delay = Duration::from_millis(delay_ms);
                observe(AgentEvent::ProviderRetry {
                    attempt: attempt + 1,
                    max_retries: 2,
                    delay_ms: delay.as_millis() as u64,
                });
                tokio::select! {
                    () = tokio::time::sleep(delay) => {},
                    () = stop.cancelled() => {
                        evidence.outcome = Err(AgentError::Cancelled);
                        return evidence;
                    },
                }
            }
            _ => return evidence,
        }
    }
    unreachable!("bounded retry loop returns on its final attempt")
}

async fn generate_once<F>(
    model: &Arc<dyn ModelService>,
    request: ModelRequest,
    stop: &CancellationToken,
    observe: &mut F,
) -> GenerationAttempt
where
    F: FnMut(AgentEvent) + Send,
{
    let route = request.route.clone();
    let started = tokio::time::Instant::now();
    let (outcome, output_observed) = stream_response(model, request, stop, observe).await;
    GenerationAttempt {
        outcome,
        route,
        output: if output_observed {
            OutputCustody::Observed
        } else {
            OutputCustody::Unobserved
        },
        timing: started..tokio::time::Instant::now(),
    }
}

async fn stream_response<F>(
    model: &Arc<dyn ModelService>,
    request: ModelRequest,
    stop: &CancellationToken,
    observe: &mut F,
) -> (Result<ModelResponse, AgentError>, bool)
where
    F: FnMut(AgentEvent) + Send,
{
    let stream = tokio::select! {
        // A service may dispatch while constructing its future. Do not even
        // call it when cancellation is already ready at this boundary.
        biased;
        () = stop.cancelled() => return (Err(AgentError::Cancelled), false),
        result = async { model.stream(request).await } => match result {
            Ok(stream) => stream,
            Err(error) if error.kind == ProviderErrorKind::ReplayContextChanged => {
                return (Err(AgentError::ReplayContextChanged), false);
            }
            Err(error) => return (Err(AgentError::Provider(error)), false),
        },
    };
    tokio::pin!(stream);
    let mut output_observed = false;
    let mut text_observed = false;
    loop {
        let event = tokio::select! {
            item = stream.next() => item,
            () = stop.cancelled() => return (Err(AgentError::Cancelled), output_observed),
        };
        match event {
            Some(Ok(ModelStreamEvent::TextDelta(text))) if !text.is_empty() => {
                output_observed = true;
                text_observed = true;
                observe(AgentEvent::TextDelta(text));
            }
            Some(Ok(ModelStreamEvent::ThinkingDelta { block, text })) if !text.is_empty() => {
                output_observed = true;
                observe(AgentEvent::ThinkingDelta { block, text });
            }
            Some(Ok(ModelStreamEvent::OutputObserved | ModelStreamEvent::ToolCall(_))) => {
                if !output_observed {
                    output_observed = true;
                    observe(AgentEvent::ModelOutputObserved);
                }
            }
            // Accounting and empty deltas do not establish generated-output
            // custody. In particular, Anthropic reports input usage at start.
            Some(Ok(
                ModelStreamEvent::Usage(_)
                | ModelStreamEvent::TextDelta(_)
                | ModelStreamEvent::ThinkingDelta { .. },
            )) => {}
            Some(Ok(ModelStreamEvent::ProviderReplayNotice {
                action,
                reason,
                count,
            })) => {
                observe(AgentEvent::ProviderReplayNotice {
                    action,
                    reason,
                    count,
                });
            }
            Some(Ok(ModelStreamEvent::Completed(response))) => {
                // Services may deliver generated content only at completion.
                // Usage alone does not establish generated-output custody.
                let generated = response.message.provider_replay.is_some()
                    || response.message.content.iter().any(|part| match part {
                        Content::Text(text) | Content::Thinking(text) => !text.is_empty(),
                        _ => true,
                    });
                if generated && !output_observed {
                    output_observed = true;
                    observe(AgentEvent::ModelOutputObserved);
                }
                // Completion-only services still supplied provisional visible
                // text, including on incomplete responses that cannot commit.
                if !text_observed && response.message.role == ion_ai::Role::Assistant {
                    let text = response
                        .message
                        .content
                        .iter()
                        .filter_map(|part| {
                            if let Content::Text(text) = part {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        observe(AgentEvent::TextDelta(text));
                    }
                }
                return (Ok(response), output_observed);
            }
            Some(Err(error)) => return (Err(AgentError::Provider(error)), output_observed),
            None => return (Err(AgentError::IncompleteModelResponse), output_observed),
        }
        // A buffered provider stream can otherwise emit an entire SSE batch in
        // one poll. Let clients publish progress and process controls between
        // events instead of exhausting their bounded callback queues.
        tokio::task::yield_now().await;
    }
}

pub(crate) async fn refresh_prompt_cache(
    model: &Arc<dyn ModelService>,
    mut request: ModelRequest,
    stop: &CancellationToken,
) -> Option<(ModelExecution, Usage)> {
    request.controls.max_output_tokens = 1;
    request.route.reason = ModelRouteReason::Auxiliary;
    let route = request.route.clone();
    let stream = tokio::select! {
        biased;
        () = stop.cancelled() => return None,
        result = async { model.stream(request).await } => result.ok()?,
    };
    tokio::pin!(stream);
    loop {
        let event = tokio::select! {
            event = stream.next() => event,
            () = stop.cancelled() => return None,
        };
        match event {
            Some(Ok(ModelStreamEvent::Completed(response))) => {
                let execution = ModelExecution {
                    route,
                    returned_model: response.returned_model,
                };
                return Some((execution, response.usage));
            }
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return None,
        }
    }
}

fn retryable_provider_error(error: &ProviderError) -> bool {
    matches!(
        error.kind,
        ProviderErrorKind::Transport
            | ProviderErrorKind::Timeout
            | ProviderErrorKind::RateLimited
            | ProviderErrorKind::Overloaded
            | ProviderErrorKind::Server
    )
}
