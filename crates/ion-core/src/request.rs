//! Request planning, durable admission and request-bound execution.
use std::sync::Arc;

use ion_ai::{
    Content, GenerationControls, ModelExecution, ModelRef, ModelRequest, ModelRoute,
    ModelRouteReason, ModelService, Reasoning, ToolChoice, ToolSpec, Usage,
};
use tokio_util::sync::CancellationToken;

use crate::{
    agent::{AgentError, AgentEvent, AgentLimits},
    generation::{GeneratedResponse, GenerationAttempt, generate_with_retry},
    session::{
        AuthorityScope, ModelContextSnapshot, RecoveryBinding, RequestBasis, Session, SessionError,
    },
    tool_set::{ToolCatalog, ToolSet},
};

pub(crate) mod calibration;
#[cfg(test)]
mod calibration_tests;
pub(crate) use calibration::{RequestCalibration, RequestMeasurement, RequestScope};

pub(crate) struct RequestPlan {
    turn: u64,
    request: ModelRequest,
    catalog: ToolCatalog,
    measurement: RequestMeasurement,
    scope: RequestScope,
    limits: AgentLimits,
    output_budget: Option<u32>,
}

pub(crate) struct IssuedRequest {
    pub attempt: GenerationAttempt,
    pub plan: RequestPlan,
}

pub(crate) struct RequestStep {
    pub generated: GeneratedResponse,
    pub plan: RequestPlan,
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

impl RequestPlan {
    pub fn new(
        session: &Session,
        basis: RequestBasis,
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
        let turn = basis.turn.ok_or(SessionError::InvalidHistory)?;
        let previous: &[ToolSpec] = basis
            .previous
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
        let context_timeline = basis.timeline(&context);
        let request = ModelRequest {
            messages: basis.messages,
            context_timeline,
            route,
            provider_session_id: Some(session.provider_session_id().to_string()),
            instructions: Some(context.instructions),
            tools: context.tools,
            prompt_cache: ion_ai::PromptCacheIntent::Reusable,
            controls: limits.controls(basis.reasoning, true),
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
        let output_budget = limits.output_budget(
            measurement.retained_bytes(),
            estimated_input,
            limits.max_output_tokens,
        );
        Ok(Self {
            turn,
            request,
            catalog,
            measurement,
            scope,
            limits,
            output_budget,
        })
    }

    /// Publish a compatible total-input sample only after the assistant exchange
    /// was committed. Failed validation/commit and auxiliary requests never call
    /// this path.
    pub async fn record_observed_input(
        &self,
        io: &crate::SessionIo,
        execution: &ModelExecution,
        usage: Usage,
    ) -> Result<(), SessionError> {
        let scope = self
            .scope
            .clone()
            .with_model(execution.route.effective.clone());
        if let Some(calibration) = RequestCalibration::new(&self.measurement, scope, usage) {
            io.run(move |session| session.set_request_calibration(calibration))
                .await?;
        }
        Ok(())
    }

    pub fn output_budget(&self) -> Option<u32> {
        self.output_budget
    }

    pub fn output_reserve(&self) -> u32 {
        self.limits
            .preferred_output_reserve(self.measurement.fixed_input_tokens())
    }

    pub fn keep_bytes(&self) -> usize {
        self.limits.compaction_keep_bytes(
            self.measurement.fixed_input_tokens() + u64::from(self.output_reserve()),
        )
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
        io: &crate::SessionIo,
        model: &Arc<dyn ModelService>,
        stop: &CancellationToken,
        observe: &mut F,
    ) -> Result<IssuedRequest, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        self.request.controls.max_output_tokens =
            self.output_budget.ok_or(AgentError::ContextTooLarge)?;
        model.validate_controls(&self.request.route.effective, &self.request.controls)?;
        let turn = self.turn;
        let model_ref = self.request.route.effective.clone();
        let context = ModelContextSnapshot {
            instructions: self
                .request
                .instructions
                .clone()
                .expect("agent instructions"),
            tools: self.request.tools.clone(),
        };
        let measurement = self.measurement.clone();
        let scope = self.scope.clone();
        io.run(move |session| {
            session.admit_request(turn, model_ref, context)?;
            session.activate_request_calibration(&measurement, &scope)
        })
        .await?;
        let attempt = generate_with_retry(model, &self.request, stop, observe, |observed| {
            io.run_with(move |session| {
                session
                    .record_uncommitted_usage(
                        crate::session::ModelUsagePurpose::Assistant,
                        observed.execution,
                        observed.usage,
                    )
                    .map_err(AgentError::from)
            })
        })
        .await;
        // Retain the route actually used by pre-output retry for cache warming.
        self.request.route = attempt.route.clone();
        Ok(IssuedRequest {
            attempt,
            plan: self,
        })
    }
}

impl AgentLimits {
    /// A planning target, not an admission minimum. Split the capacity left
    /// after fixed instructions/tools so large prompts and small windows still
    /// leave room for messages rather than demanding an impossible ceiling.
    pub(crate) fn preferred_output_reserve(self, fixed_input_tokens: u64) -> u32 {
        self.context_window_tokens.map_or(0, |capacity| {
            (u64::from(capacity).saturating_sub(fixed_input_tokens) / 2)
                .min(u64::from(self.max_output_tokens)) as u32
        })
    }

    pub(crate) fn compaction_keep_bytes(self, reserved_tokens: u64) -> usize {
        let model_budget = self.context_window_tokens.map_or(usize::MAX, |tokens| {
            (u64::from(tokens).saturating_sub(reserved_tokens) as usize).saturating_mul(3)
        });
        self.max_request_bytes.min(model_budget) / 2
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
        let Some(capacity) = self.context_window_tokens else {
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
