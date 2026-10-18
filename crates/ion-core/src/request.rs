//! Request preparation, durable admission and request-bound execution.
use std::sync::Arc;

use ion_ai::{
    Content, GenerationControls, ModelRef, ModelRequest, ModelRoute, ModelRouteReason,
    ModelService, PromptCacheIntent, Reasoning, ToolChoice, ToolSpec,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::{AgentError, AgentEvent, AgentLimits},
    generation::{GeneratedResponse, generate_with_retry},
    json_size::encoded_len,
    prompt_size::NeutralMessages,
    session::{AuthorityScope, ModelContextSnapshot, RecoveryBinding, Session, SessionError},
    tool_set::{ToolCatalog, ToolSet},
};

pub(crate) struct PreparedRequest {
    turn: u64,
    request: ModelRequest,
    catalog: ToolCatalog,
}

pub(crate) struct RequestStep {
    pub generated: GeneratedResponse,
    pub prepared: PreparedRequest,
    pub started: tokio::time::Instant,
}

pub(crate) struct RequestSettings<'a> {
    pub model: &'a ModelRef,
    pub instructions: &'a str,
    pub limits: AgentLimits,
    pub authority: Option<&'a AuthorityScope>,
}
impl RequestSettings<'_> {
    pub fn binding(
        &self,
        session: &Session,
        catalog: &ToolCatalog,
    ) -> Result<Option<RecoveryBinding>, SessionError> {
        use sha2::{Digest, Sha256};
        self.authority
            .map(|authority| {
                let bytes = serde_json::to_vec(&(
                    session.cwd(),
                    self.model,
                    session.reasoning()?,
                    self.instructions,
                    self.limits,
                    catalog.authority_manifest(),
                ))?;
                Ok(RecoveryBinding {
                    session: session.id(),
                    authority: authority.clone(),
                    fingerprint: Sha256::digest(bytes).into(),
                })
            })
            .transpose()
    }
}

impl PreparedRequest {
    pub fn new(
        session: &Session,
        turn: u64,
        tools: &ToolSet,
        reason: ModelRouteReason,
        settings: RequestSettings<'_>,
        catalog: Option<&ToolCatalog>,
    ) -> Result<Self, AgentError> {
        let RequestSettings {
            model,
            instructions,
            limits,
            ..
        } = settings;
        let previous = session.model_context()?;
        let previous: &[ToolSpec] = previous
            .as_ref()
            .map_or(&[], |context| context.tools.as_slice());
        let catalog = catalog.map_or_else(
            || tools.snapshot_with_previous(previous),
            |catalog| catalog.with_previous(previous),
        );
        let context = ModelContextSnapshot {
            instructions: instructions.into(),
            tools: catalog.declared_specs(),
        };
        let route = ModelRoute::direct(model.clone(), reason);
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
            turn,
            request,
            catalog,
        })
    }

    pub fn output_budget(&self) -> u32 {
        self.request.controls.max_output_tokens
    }

    pub fn reasoning(&self) -> Reasoning {
        self.request.controls.reasoning
    }

    pub fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }

    pub fn warming_request(&self) -> &ModelRequest {
        &self.request
    }

    pub async fn issue<F>(
        mut self,
        session: &Session,
        model: &Arc<dyn ModelService>,
        stop: &CancellationToken,
        observe: &mut F,
    ) -> Result<RequestStep, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        model.validate_controls(&self.request.route.effective, &self.request.controls)?;
        session.admit_request(
            self.turn,
            self.request.route.effective.clone(),
            ModelContextSnapshot {
                instructions: self
                    .request
                    .instructions
                    .clone()
                    .expect("agent instructions"),
                tools: self.request.tools.clone(),
            },
        )?;
        let started = tokio::time::Instant::now();
        let generated = generate_with_retry(model, &self.request, stop, observe).await?;
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
        struct LatestRequest<'a, M> {
            route: &'a ModelRoute,
            provider_session_id: &'a Option<String>,
            instructions: &'a Option<String>,
            messages: M,
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
        let latest = LatestRequest {
            route,
            provider_session_id,
            instructions,
            messages,
            tools,
            context_timeline: None,
            prompt_cache,
            controls,
        };
        // Bound the retained representation, including opaque continuation, even
        // though it is not an additive part of the neutral token estimate.
        let bytes = encoded_len(&latest)?;
        if bytes > self.max_request_bytes {
            return Ok(None);
        }
        let neutral_bytes = if messages
            .iter()
            .any(|message| message.provider_replay.is_some())
        {
            encoded_len(&LatestRequest {
                route,
                provider_session_id,
                instructions,
                messages: NeutralMessages(messages),
                tools,
                context_timeline: None,
                prompt_cache,
                controls,
            })?
        } else {
            bytes
        };
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
            neutral_bytes.saturating_sub(encoded_images).div_ceil(3) as u64 + image_count * 16_384;
        Ok(self.output_budget(bytes, estimated_input, ceiling))
    }
}
