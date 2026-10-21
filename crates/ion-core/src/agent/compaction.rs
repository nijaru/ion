//! Bounded settled-prefix summarization through the shared generation lifecycle.
use std::sync::Arc;

use ion_ai::{
    Content, IncompleteReason, ModelExecution, ModelRequest, ModelRoute, ModelRouteReason,
    ResponseTermination, Role,
};
use tokio_util::sync::CancellationToken;

use super::{Agent, AgentError, AgentEvent, contained_observer, observation_result};
use crate::{
    generation::{GeneratedResponse, context_length_exceeded, generate_with_retry},
    session::Session,
};

impl Agent {
    /// Summarize a settled prefix while retaining the complete raw Session.
    pub fn compact<F>(
        &self,
        session: Arc<Session>,
        stop: CancellationToken,
        observe: F,
    ) -> crate::Operation<bool, AgentError>
    where
        F: FnMut(AgentEvent) + Send + 'static,
    {
        let agent = self.clone();
        session.operations.start(stop.clone(), {
            let session = session.clone();
            async move {
                let (mut observe, failed) = contained_observer(observe, stop.clone());
                let result = agent.compact_owned(&session, stop, &mut observe).await;
                observation_result(result, failed.load(std::sync::atomic::Ordering::Relaxed))
            }
        })
    }

    async fn compact_owned<F>(
        &self,
        session: &Session,
        stop: CancellationToken,
        observe: &mut F,
    ) -> Result<bool, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        if stop.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        self.validate_reasoning(session.reasoning()?)?;
        let keep_bytes = self.keep_bytes().min(
            crate::json_size::encoded_len(&crate::prompt_size::NeutralMessages(
                &session.context_messages()?,
            ))? / 2,
        );
        let mut changed = false;
        loop {
            match self
                .compact_inner(session, &stop, keep_bytes, observe)
                .await?
            {
                Some(chunked) => {
                    changed = true;
                    if !chunked {
                        return Ok(true);
                    }
                }
                None => return Ok(changed),
            }
        }
    }

    pub(super) fn keep_bytes(&self) -> usize {
        let model_budget = self
            .limits
            .usable_context_tokens()
            .map_or(usize::MAX, |tokens| (tokens as usize).saturating_mul(3));
        self.limits.max_request_bytes.min(model_budget) / 2
    }

    pub(super) async fn compact_inner<F>(
        &self,
        session: &Session,
        stop: &CancellationToken,
        keep_bytes: usize,
        observe: &mut F,
    ) -> Result<Option<bool>, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let output_tokens = self.limits.max_output_tokens.min(4096);
        let mut max_through_entry = u64::MAX;
        let (through_entry, chunked, generated) = loop {
            if stop.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let Some(plan) = session.compaction_plan(
                keep_bytes,
                self.limits.max_request_bytes,
                self.limits.image_input,
                max_through_entry,
            )?
            else {
                return if session
                    .compaction_plan(keep_bytes, usize::MAX, self.limits.image_input, u64::MAX)?
                    .is_some()
                {
                    Err(AgentError::ContextTooLarge)
                } else {
                    Ok(None)
                };
            };
            let mut request = ModelRequest {
                route: ModelRoute::direct(self.model.clone(), ModelRouteReason::Auxiliary),
                provider_session_id: Some(session.provider_session_id().to_string()),
                instructions: Some(crate::summary::INSTRUCTIONS.into()),
                messages: vec![crate::summary::input(
                    plan.messages,
                    self.limits.image_input,
                )?],
                tools: Vec::new(),
                context_timeline: None,
                prompt_cache: ion_ai::PromptCacheIntent::Default,
                controls: self.limits.controls(session.reasoning()?, false),
            };
            let Some(available) = self.limits.request_output_budget(&request, output_tokens)?
            else {
                max_through_entry = plan
                    .smaller_through_entry
                    .ok_or(AgentError::ContextTooLarge)?;
                continue;
            };
            request.controls.max_output_tokens = available;
            let mut emitted_output = false;
            let generated = generate_with_retry(&self.service, &request, stop, &mut |event| {
                emitted_output |= matches!(
                    event,
                    AgentEvent::TextDelta(_)
                        | AgentEvent::ThinkingDelta { .. }
                        | AgentEvent::ModelOutputObserved
                );
            })
            .await;
            let overflow = context_length_exceeded(generated.as_ref());
            // Estimates can miss provider capacity. Halve the remaining settled
            // cuts, never replay a partial summary or change the model/authority.
            if overflow
                && !emitted_output
                && let Some(smaller) = plan.smaller_through_entry
            {
                observe(AgentEvent::ContextCompactionRetry {
                    through_entry: plan.through_entry,
                    next_through_entry: smaller,
                });
                max_through_entry = smaller;
                continue;
            }
            break (plan.through_entry, plan.chunked, generated?);
        };
        let GeneratedResponse { response, route } = generated;
        if let ResponseTermination::Incomplete(reason) = &response.termination {
            let reason = match reason {
                IncompleteReason::MaxOutputTokens => "model output limit reached",
                IncompleteReason::ContextLength => "model context limit reached",
                IncompleteReason::ContentFilter => "provider content filter interrupted response",
                IncompleteReason::Other(_) => "provider ended response before completion",
            };
            return Err(AgentError::InvalidSummary(reason));
        }
        if response.message.role != Role::Assistant {
            return Err(AgentError::InvalidSummary(
                "response is not an assistant message",
            ));
        }
        if response
            .message
            .content
            .iter()
            .any(|part| !matches!(part, Content::Text(_) | Content::Thinking(_)))
        {
            return Err(AgentError::InvalidSummary(
                "response contains non-text content",
            ));
        }
        let summary = response
            .message
            .content
            .iter()
            .filter_map(|part| match part {
                Content::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if summary.trim().is_empty() {
            return Err(AgentError::InvalidSummary(
                "response contains no summary text",
            ));
        }
        if stop.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let execution = ModelExecution {
            route,
            returned_model: response.returned_model,
        };
        session.record_compaction(through_entry, summary, execution, response.usage)?;
        observe(AgentEvent::ContextCompacted { through_entry });
        Ok(Some(chunked))
    }
}
