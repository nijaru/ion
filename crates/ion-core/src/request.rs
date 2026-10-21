//! Request preparation, durable admission and request-bound execution.
use std::sync::Arc;

use ion_ai::{
    Content, GenerationControls, ModelExecution, ModelRef, ModelRequest, ModelRoute,
    ModelRouteReason, ModelService, Reasoning, ToolChoice, ToolSpec, Usage,
};
use tokio_util::sync::CancellationToken;

use crate::{
    agent::{AgentError, AgentEvent, AgentLimits},
    generation::{GeneratedResponse, generate_with_retry},
    session::{AuthorityScope, ModelContextSnapshot, RecoveryBinding, Session, SessionError},
    tool_set::{ToolCatalog, ToolSet},
};

pub(crate) mod calibration;
#[cfg(test)]
mod calibration_tests;
pub(crate) use calibration::{RequestCalibration, RequestMeasurement, RequestScope};

pub(crate) struct PreparedRequest {
    turn: u64,
    request: ModelRequest,
    catalog: ToolCatalog,
    measurement: RequestMeasurement,
    scope: RequestScope,
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
        service: &Arc<dyn ModelService>,
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
        let scope = RequestScope::new(service, &request)?;
        let calibration = session.request_calibration_for(&scope)?;
        let measurement = RequestMeasurement::measure(&request, calibration.as_ref())?;
        let estimated_input = calibration.as_ref().map_or_else(
            || measurement.fallback_input_tokens(),
            |calibration| calibration.estimate(&measurement),
        );
        request.controls.max_output_tokens = limits
            .output_budget(
                measurement.retained_bytes(),
                estimated_input,
                limits.max_output_tokens,
            )
            .ok_or(AgentError::ContextTooLarge)?;
        Ok(Self {
            turn,
            request,
            catalog,
            measurement,
            scope,
        })
    }

    /// Publish a compatible total-input sample only after the assistant exchange
    /// was committed. Failed validation/commit and auxiliary requests never call
    /// this path.
    pub fn record_observed_input(
        &self,
        session: &Session,
        execution: &ModelExecution,
        usage: Usage,
    ) -> Result<(), SessionError> {
        let scope = self
            .scope
            .clone()
            .with_model(execution.route.effective.clone());
        if let Some(calibration) = RequestCalibration::new(&self.measurement, scope, usage) {
            session.set_request_calibration(calibration)?;
        }
        Ok(())
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

    pub fn request(&self) -> &ModelRequest {
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
        session.activate_request_calibration(&self.measurement, &self.scope)?;
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
    pub(crate) fn usable_context_tokens(self) -> Option<u32> {
        self.context_window_tokens.map(|window| {
            // Keep the established reserve where it fits, but do not consume
            // more than half of a small model's entire context as headroom.
            window - (window / 2).min(8_192)
        })
    }

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
        let Some(capacity) = self.usable_context_tokens() else {
            return Some(ceiling);
        };
        let available = u64::from(capacity).saturating_sub(estimated_input);
        (available > 0).then(|| ceiling.min(available as u32))
    }

    pub(crate) fn request_output_budget(
        self,
        request: &ModelRequest,
        ceiling: u32,
    ) -> Result<Option<u32>, serde_json::Error> {
        let measurement = RequestMeasurement::measure(request, None)?;
        Ok(self.output_budget(
            measurement.retained_bytes(),
            measurement.fallback_input_tokens(),
            ceiling,
        ))
    }
}
