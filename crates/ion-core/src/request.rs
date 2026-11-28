//! Request planning, durable admission and request-bound execution.
use std::sync::Arc;

use ion_ai::{
    GenerationControls, MessageRoles, ModelContextEncoding, ModelExecution, ModelRef, ModelRequest,
    ModelRoute, ModelRouteReason, ModelService, Reasoning, ToolChoice, ToolSpec, Usage,
};
use tokio_util::sync::CancellationToken;

use crate::{
    agent::{AgentError, AgentEvent, AgentLimits},
    generation::{GeneratedResponse, GenerationAttempt, generate_with_retry},
    session::{
        AuthorityScope, ContextReportBasis, ModelContextSnapshot, RecoveryBinding, RequestBasis,
        Session, SessionError,
    },
    tool_set::{ToolCatalog, ToolSet},
};

pub(crate) mod calibration;
#[cfg(test)]
mod calibration_tests;
pub(crate) use calibration::{RequestCalibration, RequestHead, RequestMeasurement, RequestScope};

/// Local planning snapshot, not an admission or an in-flight request budget.
/// Excludes drafts and uncommitted inputs; a Turn may retain a private frozen
/// catalog or compact/refresh before its next request. Byte counts use Core's
/// retained-request surrogate, not the provider's HTTP wire size.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextReport {
    pub model: ModelRef,
    pub revision: u64,
    pub turn: Option<u64>,
    pub context_window_tokens: Option<u32>,
    pub estimated_input_tokens: u64,
    pub basis: ContextEstimateBasis,
    pub retained_request_bytes: usize,
    pub max_request_bytes: usize,
    pub capacity: ContextCapacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ContextEstimateBasis {
    Calibrated,
    BytesFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ContextCapacity {
    Available { max_output_tokens: u32 },
    RequestBytesExceeded,
    ContextWindowExhausted,
    OutputDisabled,
}

impl ContextCapacity {
    pub fn output_tokens(self) -> Option<u32> {
        match self {
            Self::Available { max_output_tokens } => Some(max_output_tokens),
            _ => None,
        }
    }
}

pub(crate) struct RequestPlan {
    turn: u64,
    prepared: PreparedRequest,
}

/// Projection and measurement are shared by passive inspection and issuance;
/// only a RequestPlan binds this candidate to an admitted logical Turn.
pub(crate) struct PreparedRequest {
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

struct PlannedMetrics {
    measurement: RequestMeasurement,
    scope: RequestScope,
    output_budget: Option<u32>,
    estimated_input_tokens: u64,
    basis: ContextEstimateBasis,
}

fn plan_metrics(
    head: RequestHead<'_>,
    context_timeline: Option<&ion_ai::ModelContextTimeline>,
    message_roles: MessageRoles<'_>,
    message_bytes: &[crate::prompt_size::MessageBytes],
    calibration: Option<RequestCalibration>,
    service: &Arc<dyn ModelService>,
    limits: AgentLimits,
) -> Result<PlannedMetrics, AgentError> {
    if !limits.image_input && message_bytes.iter().any(|bytes| bytes.images.count > 0) {
        return Err(AgentError::ImagesUnsupported);
    }
    let scope = RequestScope::from_head(service, head)?;
    let calibration = calibration.filter(|sample| sample.matches_scope(&scope));
    let context_overhead = service.context_encoding_overhead(ModelContextEncoding {
        instructions: head.instructions,
        tools: head.tools,
        context_timeline,
        controls: head.controls,
        message_roles,
    })?;
    let measurement = RequestMeasurement::from_parts(
        head,
        message_bytes,
        calibration.as_ref(),
        context_overhead,
    )?;
    let calibration = calibration.filter(|sample| sample.applies(&measurement, &scope));
    let estimated_input_tokens = calibration.as_ref().map_or_else(
        || measurement.fallback_input_tokens(),
        |calibration| calibration.estimate(&measurement),
    );
    let basis = if calibration.is_some() {
        ContextEstimateBasis::Calibrated
    } else {
        ContextEstimateBasis::BytesFallback
    };
    let output_budget = limits.output_budget(
        measurement.retained_bytes(),
        estimated_input_tokens,
        limits.max_output_tokens,
    );
    Ok(PlannedMetrics {
        measurement,
        scope,
        output_budget,
        estimated_input_tokens,
        basis,
    })
}

impl PreparedRequest {
    pub(crate) fn new(
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
        let previous: &[ToolSpec] = basis
            .previous
            .as_ref()
            .map_or(&[], |context| context.tools.as_slice());
        let catalog = catalog.map_or_else(
            || tools.request_catalog(previous),
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
        assert_eq!(
            request.messages.len(),
            basis.message_bytes.len(),
            "byte counts belong to the frozen request"
        );
        let metrics = plan_metrics(
            RequestHead::from_request(&request),
            request.context_timeline.as_ref(),
            MessageRoles::Messages(&request.messages),
            &basis.message_bytes,
            basis.calibration,
            service,
            limits,
        )?;
        Ok(Self {
            request,
            catalog,
            measurement: metrics.measurement,
            scope: metrics.scope,
            limits,
            output_budget: metrics.output_budget,
        })
    }

    pub(crate) fn context_report(
        session: &Session,
        basis: ContextReportBasis,
        service: &Arc<dyn ModelService>,
        tools: &ToolSet,
        reason: ModelRouteReason,
        settings: RequestSettings<'_>,
    ) -> Result<ContextReport, AgentError> {
        let RequestSettings {
            model,
            instructions,
            limits,
            ..
        } = settings;
        let previous: &[ToolSpec] = basis
            .previous
            .as_ref()
            .map_or(&[], |context| context.tools.as_slice());
        let catalog = tools.request_catalog(previous);
        let context = ModelContextSnapshot {
            instructions: instructions.into(),
            tools: catalog.declared_specs(),
        };
        let route = ModelRoute::direct(model.clone(), reason);
        let controls = limits.controls(basis.reasoning, true);
        let context_timeline = basis.timeline(&context);
        let provider_session_id = session.provider_session_id().to_string();
        assert_eq!(
            basis.roles.len(),
            basis.message_bytes.len(),
            "byte counts belong to the projected roles"
        );
        let metrics = plan_metrics(
            RequestHead {
                route: &route,
                provider_session_id: Some(&provider_session_id),
                instructions: Some(&context.instructions),
                tools: &context.tools,
                prompt_cache: ion_ai::PromptCacheIntent::Reusable,
                controls: &controls,
            },
            context_timeline.as_ref(),
            MessageRoles::Roles(&basis.roles),
            &basis.message_bytes,
            basis.calibration,
            service,
            limits,
        )?;
        Ok(ContextReport {
            model: route.effective,
            revision: basis.revision,
            turn: basis.turn,
            context_window_tokens: limits.context_window_tokens,
            estimated_input_tokens: metrics.estimated_input_tokens,
            basis: metrics.basis,
            retained_request_bytes: metrics.measurement.retained_bytes(),
            max_request_bytes: limits.max_request_bytes,
            capacity: limits.context_capacity(
                metrics.measurement.retained_bytes(),
                metrics.estimated_input_tokens,
                limits.max_output_tokens,
            ),
        })
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
        let turn = basis.turn.ok_or(SessionError::InvalidHistory)?;
        Ok(Self {
            turn,
            prepared: PreparedRequest::new(
                session, basis, service, tools, reason, settings, catalog,
            )?,
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
            .prepared
            .scope
            .clone()
            .with_model(execution.route.effective.clone());
        if let Some(calibration) = RequestCalibration::new(&self.prepared.measurement, scope, usage)
        {
            io.run(move |session| session.set_request_calibration(calibration))
                .await?;
        }
        Ok(())
    }

    pub fn output_budget(&self) -> Option<u32> {
        self.prepared.output_budget
    }

    pub fn output_reserve(&self) -> u32 {
        self.prepared
            .limits
            .preferred_output_reserve(self.prepared.measurement.fixed_input_tokens())
    }

    pub fn keep_bytes(&self) -> usize {
        self.prepared.limits.compaction_keep_bytes(
            self.prepared.measurement.fixed_input_tokens() + u64::from(self.output_reserve()),
        )
    }

    pub fn reasoning(&self) -> Reasoning {
        self.prepared.request.controls.reasoning
    }

    pub fn catalog(&self) -> &ToolCatalog {
        &self.prepared.catalog
    }

    pub fn request(&self) -> &ModelRequest {
        &self.prepared.request
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
        let prepared = &mut self.prepared;
        prepared.request.controls.max_output_tokens =
            prepared.output_budget.ok_or(AgentError::ContextTooLarge)?;
        model.validate_controls(
            &prepared.request.route.effective,
            &prepared.request.controls,
        )?;
        let turn = self.turn;
        let model_ref = prepared.request.route.effective.clone();
        let context = ModelContextSnapshot {
            instructions: prepared
                .request
                .instructions
                .clone()
                .expect("agent instructions"),
            tools: prepared.request.tools.clone(),
        };
        let measurement = prepared.measurement.clone();
        let scope = prepared.scope.clone();
        io.run(move |session| {
            session.admit_request(turn, model_ref, context)?;
            session.activate_request_calibration(&measurement, &scope)
        })
        .await?;
        let attempt =
            generate_with_retry(model, &mut prepared.request, stop, observe, |observed| {
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
        prepared.request.route = attempt.route.clone();
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
        self.context_capacity(bytes, estimated_input, ceiling)
            .output_tokens()
    }

    fn context_capacity(self, bytes: usize, estimated_input: u64, ceiling: u32) -> ContextCapacity {
        if bytes > self.max_request_bytes {
            return ContextCapacity::RequestBytesExceeded;
        }
        if ceiling == 0 {
            return ContextCapacity::OutputDisabled;
        }
        let max_output_tokens = match self.context_window_tokens {
            None => ceiling,
            Some(capacity) => {
                let available = u64::from(capacity).saturating_sub(estimated_input);
                if available == 0 {
                    return ContextCapacity::ContextWindowExhausted;
                }
                ceiling.min(available as u32)
            }
        };
        ContextCapacity::Available { max_output_tokens }
    }

    pub(crate) fn request_output_budget(
        self,
        request: &ModelRequest,
        ceiling: u32,
        context_overhead: usize,
    ) -> Result<Option<u32>, serde_json::Error> {
        let measurement = RequestMeasurement::measure(request, None, context_overhead)?;
        Ok(self.output_budget(
            measurement.retained_bytes(),
            measurement.fallback_input_tokens(),
            ceiling,
        ))
    }
}
