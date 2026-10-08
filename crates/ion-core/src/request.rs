//! Coherent coding request preparation, durable admission and request-bound execution.
use std::sync::Arc;

use ion_ai::{
    Content, GenerationControls, Message, ModelRef, ModelRequest, ModelRoute, ModelRouteReason,
    ModelService, PromptCacheIntent, Reasoning, ToolChoice, ToolSpec,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::{AgentError, AgentEvent, AgentLimits},
    generation::{GeneratedResponse, generate_with_retry},
    json_size::encoded_len,
    session::{ModelContextSnapshot, Session},
    tool_set::{ToolCatalog, ToolSet},
};

pub(crate) struct PreparedRequest<'s> {
    session: &'s Session,
    turn: u64,
    request: ModelRequest,
    catalog: ToolCatalog,
}

pub(crate) struct RequestStep<'s> {
    pub generated: GeneratedResponse,
    pub prepared: PreparedRequest<'s>,
    pub started: tokio::time::Instant,
}

impl<'s> PreparedRequest<'s> {
    pub fn new(
        session: &'s Session,
        turn: u64,
        tools: &ToolSet,
        model: ModelRef,
        reason: ModelRouteReason,
        instructions: &str,
        limits: AgentLimits,
    ) -> Result<Self, AgentError> {
        let previous = session.model_context()?;
        let catalog = tools.snapshot_with_previous(
            previous
                .as_ref()
                .map_or(&[], |context| context.tools.as_slice()),
        );
        let context = ModelContextSnapshot {
            instructions: instructions.into(),
            tools: catalog.declared_specs(),
        };
        let route = ModelRoute::direct(model, reason);
        let mut request = ModelRequest {
            messages: session.context_messages_for(&route.effective)?,
            context_timeline: session.context_timeline_for(&route.effective, &context)?,
            route,
            provider_session_id: Some(session.provider_session_id().to_string()),
            instructions: Some(context.instructions),
            tools: context.tools,
            prompt_cache: ion_ai::PromptCacheIntent::Reusable,
            controls: limits.controls(session.reasoning()?, true),
        };
        if !limits.image_input
            && request
                .messages
                .iter()
                .flat_map(|message| &message.content)
                .any(|part| {
                    matches!(part, Content::Image(_))
                        || matches!(part, Content::ToolResult(result) if !result.images.is_empty())
                })
        {
            return Err(AgentError::ImagesUnsupported);
        }
        request.controls.max_output_tokens = limits
            .request_output_budget(&request, limits.max_output_tokens)?
            .ok_or(AgentError::ContextTooLarge)?;
        Ok(Self {
            session,
            turn,
            request,
            catalog,
        })
    }

    pub fn output_budget(&self) -> u32 {
        self.request.controls.max_output_tokens
    }

    pub fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    pub fn warming_request(&self) -> &ModelRequest {
        &self.request
    }

    pub async fn issue<F>(
        mut self,
        model: &Arc<dyn ModelService>,
        stop: &CancellationToken,
        observe: &mut F,
    ) -> Result<RequestStep<'s>, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        model.validate_controls(&self.request.route.effective, &self.request.controls)?;
        self.session.admit_request(
            self.turn,
            self.request.route.effective.clone(),
            ModelContextSnapshot {
                instructions: self
                    .request
                    .instructions
                    .clone()
                    .expect("coding instructions"),
                tools: self.request.tools.clone(),
            },
        )?;
        let started = tokio::time::Instant::now();
        let generated = generate_with_retry(model, self.request.clone(), stop, observe).await?;
        // Retain the route actually used by pre-output retry for cache warming.
        self.request.route = generated.route.clone();
        Ok(RequestStep {
            generated,
            prepared: self,
            started,
        })
    }
}

impl AgentLimits {
    pub(crate) fn controls(self, reasoning: Reasoning, with_tools: bool) -> GenerationControls {
        GenerationControls {
            max_output_tokens: if with_tools {
                self.max_output_tokens
            } else {
                self.max_output_tokens.min(4096)
            },
            temperature: None,
            top_p: None,
            reasoning,
            tool_choice: if with_tools {
                ToolChoice::Auto
            } else {
                ToolChoice::None
            },
            parallel_tool_calls: with_tools,
        }
    }

    pub(crate) fn output_budget(
        self,
        bytes: usize,
        estimated_input: u64,
        ceiling: u32,
    ) -> Option<u32> {
        if bytes > self.max_request_bytes || ceiling == 0 {
            return None;
        }
        let Some(window) = self.context_window_tokens else {
            return Some(ceiling);
        };
        let available = u64::from(window).saturating_sub(estimated_input + 8_192);
        (available > 0).then(|| ceiling.min(available as u32))
    }

    pub(crate) fn request_output_budget(
        self,
        request: &ModelRequest,
        ceiling: u32,
    ) -> Result<Option<u32>, serde_json::Error> {
        // Timelines optimize adapters, not the latest provider-neutral prompt.
        // Borrow only the latest snapshot; do not clone replay/images or encode
        // historical loadouts just to discard them. Exhaustive destructuring
        // makes new request fields require an explicit sizing decision.
        #[derive(Serialize)]
        struct LatestRequest<'a> {
            route: &'a ModelRoute,
            provider_session_id: &'a Option<String>,
            instructions: &'a Option<String>,
            messages: &'a [Message],
            tools: &'a [ToolSpec],
            context_timeline: Option<()>,
            prompt_cache: &'a PromptCacheIntent,
            controls: &'a GenerationControls,
        }
        let ModelRequest {
            route,
            provider_session_id,
            instructions,
            messages,
            tools,
            context_timeline: _,
            prompt_cache,
            controls,
        } = request;
        let bytes = encoded_len(&LatestRequest {
            route,
            provider_session_id,
            instructions,
            messages,
            tools,
            context_timeline: None,
            prompt_cache,
            controls,
        })?;
        let (encoded_images, image_count) = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .fold((0usize, 0u64), |(bytes, count), part| match part {
                Content::Image(image) => (bytes.saturating_add(image.data().len()), count + 1),
                Content::ToolResult(result) => result
                    .images
                    .iter()
                    .fold((bytes, count), |(bytes, count), image| {
                        (bytes.saturating_add(image.data().len()), count + 1)
                    }),
                _ => (bytes, count),
            });
        // Base64 is transport data, not prompt text. Reserve visual tokens until
        // observed provider usage supplies a more precise count.
        let estimated_input =
            bytes.saturating_sub(encoded_images).div_ceil(3) as u64 + image_count * 16_384;
        Ok(self.output_budget(bytes, estimated_input, ceiling))
    }
}
