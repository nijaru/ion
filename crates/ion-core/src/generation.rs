//! One provider request lifecycle: streaming, cancellation, and bounded pre-output retry.
use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use ion_ai::{
    ModelExecution, ModelRequest, ModelResponse, ModelRoute, ModelRouteReason, ModelService,
    ModelStreamEvent, ProviderError, ProviderErrorKind, Usage,
};
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentError, AgentEvent};

#[derive(Debug)]
pub(crate) struct GeneratedResponse {
    pub response: ModelResponse,
    pub route: ModelRoute,
}

pub(crate) async fn generate_with_retry<F>(
    model: &Arc<dyn ModelService>,
    request: ModelRequest,
    stop: &CancellationToken,
    observe: &mut F,
) -> Result<GeneratedResponse, AgentError>
where
    F: FnMut(AgentEvent) + Send,
{
    for attempt in 0..=2 {
        let mut attempt_request = request.clone();
        if attempt > 0 {
            attempt_request.route.reason = ModelRouteReason::Retry;
        }
        let route = attempt_request.route.clone();
        let (result, observed) = generate_once(model, attempt_request, stop, observe).await;
        match result {
            Err(AgentError::Provider(error))
                if !observed && attempt < 2 && retryable_provider_error(&error) =>
            {
                // A server's pacing takes precedence over local backoff.
                // Leave long waits to the caller instead of holding a Turn
                // open for an unbounded provider-requested interval.
                let delay_ms = error.retry_after_ms.unwrap_or(500 * (1 << attempt));
                if delay_ms > 60_000 {
                    return Err(AgentError::Provider(error));
                }
                let delay = Duration::from_millis(delay_ms);
                observe(AgentEvent::ProviderRetry {
                    attempt: attempt + 1,
                    max_retries: 2,
                    delay_ms: delay.as_millis() as u64,
                });
                tokio::select! {
                    () = tokio::time::sleep(delay) => {},
                    () = stop.cancelled() => return Err(AgentError::Cancelled),
                }
            }
            Ok(response) => return Ok(GeneratedResponse { response, route }),
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded retry loop returns on its final attempt")
}

async fn generate_once<F>(
    model: &Arc<dyn ModelService>,
    request: ModelRequest,
    stop: &CancellationToken,
    observe: &mut F,
) -> (Result<ModelResponse, AgentError>, bool)
where
    F: FnMut(AgentEvent) + Send,
{
    let stream = tokio::select! {
        result = model.stream(request) => match result {
            Ok(stream) => stream,
            Err(error) if error.kind == ProviderErrorKind::ReplayContextChanged => {
                return (Err(AgentError::ReplayContextChanged), false);
            }
            Err(error) => return (Err(AgentError::Provider(error)), false),
        },
        () = stop.cancelled() => return (Err(AgentError::Cancelled), false),
    };
    tokio::pin!(stream);
    let mut observed = false;
    loop {
        let event = tokio::select! {
            item = stream.next() => item,
            () = stop.cancelled() => return (Err(AgentError::Cancelled), observed),
        };
        match event {
            Some(Ok(ModelStreamEvent::TextDelta(text))) => {
                observed = true;
                observe(AgentEvent::TextDelta(text));
            }
            Some(Ok(ModelStreamEvent::ToolCall(_))) | Some(Ok(ModelStreamEvent::Usage(_))) => {
                observed = true;
            }
            Some(Ok(ModelStreamEvent::ProviderReplayNotice {
                action,
                reason,
                count,
            })) => {
                observed = true;
                observe(AgentEvent::ProviderReplayNotice {
                    action,
                    reason,
                    count,
                });
            }
            Some(Ok(ModelStreamEvent::Completed(response))) => return (Ok(response), observed),
            Some(Err(error)) => return (Err(AgentError::Provider(error)), observed),
            None => return (Err(AgentError::IncompleteModelResponse), observed),
        }
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
        result = model.stream(request) => result.ok()?,
        () = stop.cancelled() => return None,
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
