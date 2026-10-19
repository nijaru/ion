//! One coding loop for library, headless and terminal clients.
use ion_ai::{
    Content, IncompleteReason, Message, ModelExecution, ModelRef, ModelRequest, ModelResponse,
    ModelRoute, ModelRouteReason, ModelService, ProviderError, ProviderErrorKind, Reasoning,
    ResponseTermination, Role,
};
#[cfg(test)]
use ion_ai::{GenerationControls, ToolChoice};
use serde_json::Value;
use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    RequestReceipt, SubmissionRequest,
    generation::{GeneratedResponse, generate_with_retry, refresh_prompt_cache},
    request::{PreparedRequest, RequestStep},
    session::{
        ModelContextSnapshot, Session, SessionError, StoredToolActivity, TurnAdmission,
        TurnEndReason,
    },
    tool_set::{ToolActivity, ToolDispatch, ToolExecution, ToolSet, ToolSource},
};

/// A durable receipt, with a new retained operation only for the first admission.
/// Reconciliation never restarts an accepted Turn, including unfinished work.
pub struct RequestSubmission {
    pub receipt: RequestReceipt,
    pub operation: Option<crate::Operation<String, AgentError>>,
}

fn contained_observer<F: FnMut(AgentEvent) + Send>(
    mut observe: F,
    stop: CancellationToken,
) -> (
    impl FnMut(AgentEvent) + Send,
    Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let failed = Arc::new(AtomicBool::new(false));
    let flag = failed.clone();
    (
        move |event| {
            if !flag.load(Ordering::Relaxed)
                && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observe(event)))
                    .is_err()
            {
                flag.store(true, Ordering::Relaxed);
                stop.cancel();
            }
        },
        failed,
    )
}

fn observation_result<T>(result: Result<T, AgentError>, failed: bool) -> Result<T, AgentError> {
    if !failed {
        return result;
    }
    match result {
        Ok(_) | Err(AgentError::Cancelled) => Err(AgentError::ObserverPanicked),
        Err(error) => Err(AgentError::ObserverSettlementFailed(Box::new(error))),
    }
}

fn user_text(prompt: String) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(prompt)],
        provider_replay: None,
    }
}

/// Host-owned prepared input and metadata waiting for a Session commit.
/// A failed write retains both for recovery; only the message enters the Session.
pub struct SteeringInbox<M = ()> {
    pending: Mutex<VecDeque<crate::AcceptedInput<M>>>,
    budget: crate::InputBudget,
    limits: AgentLimits,
}

impl<M> SteeringInbox<M> {
    pub fn new(limits: AgentLimits, budget: crate::InputBudget) -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            budget,
            limits,
        }
    }

    pub fn push_message(&self, input: Message, metadata: M) -> Result<(), AgentError>
    where
        M: serde::Serialize,
    {
        let input = self.budget.admit(input, metadata, self.limits)?;
        self.push_admitted(input);
        Ok(())
    }

    /// Move existing admission custody without releasing/re-acquiring its budget.
    pub fn push_admitted(&self, input: crate::AcceptedInput<M>) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(input);
    }

    /// Return inputs that never entered the Session. Call after joining the
    /// operation; the host can restore them to its editor or queue.
    pub fn take_uncommitted(&self) -> Vec<crate::AcceptedInput<M>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect()
    }

    fn record_pending(
        &self,
        session: &Session,
        turn: u64,
        limits: AgentLimits,
    ) -> Result<Vec<Message>, AgentError> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        validate_steering(&pending, limits)?;
        session.record_steerings(
            turn,
            pending
                .iter()
                .map(|input| input.message().clone())
                .collect(),
        )?;
        let accepted = pending.drain(..).collect::<Vec<_>>();
        drop(pending);
        // Host metadata may run user-defined Drop code. Never release it while
        // holding the inbox lock, including after the atomic Session commit.
        Ok(accepted
            .into_iter()
            .map(crate::AcceptedInput::into_message)
            .collect())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "assistant execution metadata is committed atomically with steering"
    )]
    fn record_assistant(
        &self,
        session: &Session,
        turn: u64,
        message: ion_ai::Message,
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
        usage: ion_ai::Usage,
        limits: AgentLimits,
    ) -> Result<(bool, Vec<Message>), AgentError> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_steering(&pending, limits)?;
        let complete = session.record_assistant_with_steering(
            turn,
            message,
            tool_activities,
            execution,
            usage,
            pending
                .iter()
                .map(|input| input.message().clone())
                .collect(),
        )?;
        let accepted = pending.drain(..).collect::<Vec<_>>();
        drop(pending);
        Ok((
            complete,
            accepted
                .into_iter()
                .map(crate::AcceptedInput::into_message)
                .collect(),
        ))
    }
}

impl SteeringInbox<()> {
    pub fn push(&self, prompt: String) -> Result<(), AgentError> {
        self.push_message(user_text(prompt), ())
    }
}

fn validate_steering<M>(
    pending: &VecDeque<crate::AcceptedInput<M>>,
    limits: AgentLimits,
) -> Result<(), AgentError> {
    // Embedded callers may pass an inbox admitted under another route. Validate
    // that boundary too; live clients use the active agent's limits at admission.
    for input in pending {
        crate::input::validate_input(input.message(), limits)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PromptCacheWarmingPolicy {
    /// Cache lifetime. Warming is disabled when the ten-second safety margin
    /// leaves no positive refresh point within the one-hour active horizon.
    pub lifetime_seconds: u64,
    /// Micro-US-dollars per million tokens.
    pub cache_write_microusd_per_million: u64,
    pub cache_read_microusd_per_million: u64,
    pub output_microusd_per_million: u64,
    pub minimum_savings_microusd: u64,
}

impl PromptCacheWarmingPolicy {
    fn cached_prefix_tokens(self, usage: ion_ai::Usage) -> Option<u64> {
        if usage.cache_read_input_tokens.is_none() && usage.cache_write_input_tokens.is_none() {
            return None;
        }
        let tokens = usage
            .cache_read_input_tokens
            .unwrap_or(0)
            .checked_add(usage.cache_write_input_tokens.unwrap_or(0))?;
        (tokens > 0).then_some(tokens)
    }

    fn token_cost_microusd(tokens: u64, rate_microusd_per_million: u64) -> u128 {
        u128::from(tokens).saturating_mul(u128::from(rate_microusd_per_million)) / 1_000_000
    }

    fn net_refresh_savings_microusd(self, prefix_tokens: u64) -> Option<u64> {
        let miss = Self::token_cost_microusd(prefix_tokens, self.cache_write_microusd_per_million);
        let hit = Self::token_cost_microusd(prefix_tokens, self.cache_read_microusd_per_million);
        let refresh = hit.saturating_add(Self::token_cost_microusd(
            1,
            self.output_microusd_per_million,
        ));
        let net = miss.checked_sub(hit)?.checked_sub(refresh)?;
        u64::try_from(net).ok()
    }

    fn should_warm(self, usage: ion_ai::Usage) -> bool {
        self.cached_prefix_tokens(usage)
            .and_then(|tokens| self.net_refresh_savings_microusd(tokens))
            .is_some_and(|net| net >= self.minimum_savings_microusd)
    }

    /// No immediate refresh: a lifetime within the safety margin cannot be
    /// extended economically and must not create a zero-delay request loop.
    fn refresh_after(self) -> Option<std::time::Duration> {
        let ninety_percent = self.lifetime_seconds.saturating_mul(9) / 10;
        let ten_second_margin = self.lifetime_seconds.checked_sub(10)?;
        (ten_second_margin > 0)
            .then(|| std::time::Duration::from_secs(ninety_percent.min(ten_second_margin)))
    }
}

const ACTIVE_CACHE_WARMING_LIMIT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

struct PromptCacheWarmer {
    request: ModelRequest,
    policy: PromptCacheWarmingPolicy,
    refresh_after: std::time::Duration,
    next_refresh: tokio::time::Instant,
    expires_at: tokio::time::Instant,
    stop_at: tokio::time::Instant,
}

impl PromptCacheWarmer {
    fn new(
        policy: PromptCacheWarmingPolicy,
        request: &ModelRequest,
        usage: ion_ai::Usage,
        request_started: tokio::time::Instant,
    ) -> Option<Self> {
        if !policy.should_warm(usage) {
            return None;
        }
        let refresh_after = policy.refresh_after()?;
        // Discard ineligible delays before constructing their deadlines.
        if refresh_after >= ACTIVE_CACHE_WARMING_LIMIT {
            return None;
        }
        let next_refresh = request_started + refresh_after;
        let expires_at = request_started + std::time::Duration::from_secs(policy.lifetime_seconds);
        let stop_at = request_started + ACTIVE_CACHE_WARMING_LIMIT;
        Some(Self {
            request: request.clone(),
            policy,
            refresh_after,
            next_refresh,
            expires_at,
            stop_at,
        })
    }

    fn reschedule(&mut self, usage: ion_ai::Usage, refresh_started: tokio::time::Instant) -> bool {
        if !self.policy.should_warm(usage) {
            return false;
        }
        self.next_refresh = refresh_started + self.refresh_after;
        self.expires_at =
            refresh_started + std::time::Duration::from_secs(self.policy.lifetime_seconds);
        self.next_refresh < self.stop_at
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct AgentLimits {
    pub max_request_bytes: usize,
    pub max_output_tokens: u32,
    pub context_window_tokens: Option<u32>,
    pub image_input: bool,
    pub prompt_cache_warming: Option<PromptCacheWarmingPolicy>,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 8 * 1024 * 1024,
            max_output_tokens: 16_384,
            context_window_tokens: None,
            image_input: false,
            prompt_cache_warming: None,
        }
    }
}

mod recovery;
#[cfg(test)]
mod recovery_tests;
use recovery::TurnStart;

/// One agent loop bound to a logical model and its immutable host contract.
#[derive(Clone)]
pub struct Agent {
    service: Arc<dyn ModelService>,
    model: ModelRef,
    tools: Arc<ToolSet>,
    limits: AgentLimits,
    recovery_scope: Option<crate::AuthorityScope>,
}

impl Agent {
    pub fn new(
        service: Arc<dyn ModelService>,
        tools: Arc<dyn ToolSource>,
        model: ModelRef,
    ) -> Self {
        Self::with_tool_set(service, Arc::new(ToolSet::new([tools])), model)
    }

    pub fn with_tool_set(
        service: Arc<dyn ModelService>,
        tools: Arc<ToolSet>,
        model: ModelRef,
    ) -> Self {
        Self {
            service,
            model,
            tools,
            limits: AgentLimits::default(),
            recovery_scope: None,
        }
    }

    pub fn with_limits(mut self, limits: AgentLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn model(&self) -> &ModelRef {
        &self.model
    }

    pub fn limits(&self) -> AgentLimits {
        self.limits
    }

    /// Validate a restored or prospective preference for both coding and summary
    /// requests. The service, not Core, owns wire/model-specific constraints.
    pub fn validate_reasoning(&self, reasoning: Reasoning) -> Result<(), AgentError> {
        for with_tools in [true, false] {
            self.service
                .validate_controls(&self.model, &self.limits.controls(reasoning, with_tools))?;
        }
        Ok(())
    }

    pub fn select_reasoning(
        &self,
        session: &Session,
        reasoning: Reasoning,
    ) -> Result<(), AgentError> {
        let guard = session.operations.try_lock().map_err(SessionError::from)?;
        self.validate_reasoning(reasoning)?;
        session.record_selection(&guard, self.model.clone(), Some(reasoning))?;
        Ok(())
    }

    /// Publish this agent's model only if the retained preference is supported.
    pub fn select_model(&self, session: &Session) -> Result<(), AgentError> {
        let guard = session.operations.try_lock().map_err(SessionError::from)?;
        self.validate_reasoning(session.reasoning()?)?;
        session.record_selection(&guard, self.model.clone(), None)?;
        Ok(())
    }

    pub fn tool_catalog(&self) -> crate::tool_set::ToolCatalog {
        self.tools.snapshot()
    }

    async fn execute_tool_with_cache_warming(
        &self,
        session: &Session,
        turn: u64,
        tool: ion_ai::BoxFuture<'_, Result<ToolExecution, AgentError>>,
        stop: &CancellationToken,
        warmer: &mut Option<PromptCacheWarmer>,
    ) -> Result<ToolExecution, AgentError> {
        tokio::pin!(tool);
        loop {
            let Some(state) = warmer.as_ref() else {
                return tool.await;
            };
            let deadline = state.next_refresh;
            let valid_until = state.expires_at.min(state.stop_at);
            let request = state.request.clone();
            let refresh = async {
                tokio::time::sleep_until(deadline).await;
                let started = tokio::time::Instant::now();
                // Check at dispatch, not before sleeping: a slow response,
                // refresh, or delayed timer must not restart an expired window.
                if started >= valid_until {
                    return (started, None);
                }
                let refreshed = refresh_prompt_cache(&self.service, request, stop).await;
                (started, refreshed)
            };
            tokio::pin!(refresh);
            tokio::select! {
                output = &mut tool => return output,
                refreshed = &mut refresh => {
                    let (started, Some((execution, usage))) = refreshed else {
                        *warmer = None;
                        continue;
                    };
                    // Cache warming is an optimization. A failure to publish
                    // its accounting fact must not interrupt an in-flight
                    // workspace effect; the next correctness-critical Session
                    // write will still surface storage failure.
                    let _ = session.record_cache_warm(turn, execution, usage);
                    let keep_warming = warmer
                        .as_mut()
                        .is_some_and(|state| state.reschedule(usage, started));
                    if !keep_warming {
                        *warmer = None;
                    }
                }
                () = stop.cancelled() => {
                    *warmer = None;
                    // The host owns cancellation and effect settlement. Dropping
                    // this future would interrupt its process/capture cleanup.
                    return tool.await;
                }
            }
        }
    }

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

    fn keep_bytes(&self) -> usize {
        let model_budget = self
            .limits
            .context_window_tokens
            .map_or(usize::MAX, |window| {
                (window.saturating_sub(8_192) as usize).saturating_mul(3)
            });
        self.limits.max_request_bytes.min(model_budget) / 2
    }

    async fn compact_inner<F>(
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
        let (through_entry, chunked, request) = loop {
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
            if let Some(available) = self.limits.request_output_budget(&request, output_tokens)? {
                request.controls.max_output_tokens = available;
                break (plan.through_entry, plan.chunked, request);
            }
            max_through_entry = plan
                .smaller_through_entry
                .ok_or(AgentError::ContextTooLarge)?;
        };
        let GeneratedResponse { response, route } =
            generate_with_retry(&self.service, &request, stop, &mut |_| {}).await?;
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

    pub fn submit<F>(
        &self,
        session: Arc<Session>,
        prompt: String,
        instructions: String,
        stop: CancellationToken,
        observe: F,
    ) -> crate::Operation<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send + 'static,
    {
        if prompt.trim().is_empty() {
            return crate::Operation::ready(Err(AgentError::EmptyPrompt));
        }
        self.submit_message(session, user_text(prompt), instructions, stop, observe)
    }

    /// Submit ordered text and image parts through the same durable Turn loop.
    pub fn submit_message<F>(
        &self,
        session: Arc<Session>,
        input: Message,
        instructions: String,
        stop: CancellationToken,
        observe: F,
    ) -> crate::Operation<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send + 'static,
    {
        self.start_submission::<_, ()>(session, input, instructions, stop, None, observe)
    }

    /// Accept steering at model-step boundaries. Restore uncommitted inputs
    /// only after joining, not after requesting stop.
    pub fn submit_with_steering<F, M: Send + 'static>(
        &self,
        session: Arc<Session>,
        prompt: String,
        instructions: String,
        stop: CancellationToken,
        steering: Arc<SteeringInbox<M>>,
        observe: F,
    ) -> crate::Operation<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send + 'static,
    {
        if prompt.trim().is_empty() {
            return crate::Operation::ready(Err(AgentError::EmptyPrompt));
        }
        self.submit_message_with_steering(
            session,
            user_text(prompt),
            instructions,
            stop,
            steering,
            observe,
        )
    }

    pub fn submit_message_with_steering<F, M: Send + 'static>(
        &self,
        session: Arc<Session>,
        input: Message,
        instructions: String,
        stop: CancellationToken,
        steering: Arc<SteeringInbox<M>>,
        observe: F,
    ) -> crate::Operation<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send + 'static,
    {
        self.start_submission(session, input, instructions, stop, Some(steering), observe)
    }

    /// Accept a literal message with stable delivery correlation. Only the message
    /// defines conflicting reuse; redelivery cannot change the original binding.
    /// An accepted request reconciles even while another operation is active.
    pub fn submit_request<F, M: Send + 'static>(
        &self,
        session: Arc<Session>,
        request: SubmissionRequest,
        instructions: String,
        stop: CancellationToken,
        steering: Option<Arc<SteeringInbox<M>>>,
        observe: F,
    ) -> Result<RequestSubmission, AgentError>
    where
        F: FnMut(AgentEvent) + Send + 'static,
    {
        let SubmissionRequest { key, input } = request;
        if let Some(receipt) = session.reconcile_request(&key, &input)? {
            return Ok(RequestSubmission {
                receipt,
                operation: None,
            });
        }
        let mut input = Some(input);
        let result = session.operations.start_prepared(stop.clone(), || {
            self.validate_submission(&session, input.as_ref().expect("unconsumed request"), &stop)?;
            let (binding, catalog) = self.admission_binding(&session, &instructions)?;
            let admission = session.admit_turn(
                input.take().expect("validated request"),
                self.model.clone(),
                Some(key.clone()),
                binding,
            )?;
            let (turn, interrupted) = match admission {
                TurnAdmission::Existing(receipt) => return Ok((receipt, None)),
                TurnAdmission::New { turn, interrupted } => (turn, interrupted),
            };
            let receipt = RequestReceipt {
                session: session.id(),
                key: key.clone(),
                turn,
                end: None,
            };
            let agent = self.clone();
            let session = session.clone();
            Ok((
                receipt,
                Some(async move {
                    let (mut observe, failed) = contained_observer(observe, stop.clone());
                    let result = agent
                        .run_admitted(
                            &session,
                            TurnStart {
                                turn,
                                interrupted,
                                resumed: None,
                                catalog,
                            },
                            instructions,
                            stop,
                            steering.as_deref(),
                            &mut observe,
                        )
                        .await;
                    observation_result(result, failed.load(std::sync::atomic::Ordering::Relaxed))
                }),
            ))
        });
        match result {
            Ok((receipt, operation)) => Ok(RequestSubmission { receipt, operation }),
            Err(error) => {
                // Another owner may have committed between the optimistic lookup
                // and operation reservation. Reconcile without taking its custody.
                if let Some(input) = input.as_ref()
                    && let Some(receipt) = session.reconcile_request(&key, input)?
                {
                    return Ok(RequestSubmission {
                        receipt,
                        operation: None,
                    });
                }
                Err(error)
            }
        }
    }

    fn start_submission<F, M: Send + 'static>(
        &self,
        session: Arc<Session>,
        input: Message,
        instructions: String,
        stop: CancellationToken,
        steering: Option<Arc<SteeringInbox<M>>>,
        observe: F,
    ) -> crate::Operation<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send + 'static,
    {
        let agent = self.clone();
        session.operations.start(stop.clone(), {
            let session = session.clone();
            async move {
                let (mut observe, failed) = contained_observer(observe, stop.clone());
                let result = agent
                    .submit_inner(
                        &session,
                        input,
                        instructions,
                        stop,
                        steering.as_deref(),
                        &mut observe,
                    )
                    .await;
                observation_result(result, failed.load(std::sync::atomic::Ordering::Relaxed))
            }
        })
    }

    async fn submit_inner<F, M: Send>(
        &self,
        session: &Arc<Session>,
        input: Message,
        instructions: String,
        stop: CancellationToken,
        steering: Option<&SteeringInbox<M>>,
        observe: &mut F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        self.validate_submission(session, &input, &stop)?;
        let (binding, catalog) = self.admission_binding(session, &instructions)?;
        let TurnAdmission::New { turn, interrupted } =
            session.admit_turn(input, self.model.clone(), None, binding)?
        else {
            unreachable!("anonymous input cannot reconcile");
        };
        self.run_admitted(
            session,
            TurnStart {
                turn,
                interrupted,
                resumed: None,
                catalog,
            },
            instructions,
            stop,
            steering,
            observe,
        )
        .await
    }

    fn validate_submission(
        &self,
        session: &Session,
        input: &Message,
        stop: &CancellationToken,
    ) -> Result<(), AgentError> {
        crate::input::validate_input(input, self.limits)?;
        self.validate_context(session, stop)
    }

    fn validate_context(
        &self,
        session: &Session,
        stop: &CancellationToken,
    ) -> Result<(), AgentError> {
        if stop.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        self.validate_reasoning(session.reasoning()?)?;
        if !self.limits.image_input
            && session.context_messages()?.iter().any(|message| {
                message
                    .content
                    .iter()
                    .any(|part| matches!(part, Content::Image(_)))
            })
        {
            return Err(AgentError::ImagesUnsupported);
        }
        Ok(())
    }

    async fn run_admitted<F, M: Send>(
        &self,
        session: &Arc<Session>,
        start: TurnStart,
        instructions: String,
        stop: CancellationToken,
        steering: Option<&SteeringInbox<M>>,
        observe: &mut F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        if let Some(point) = start.resumed {
            observe(AgentEvent::TurnResumed { point });
        } else {
            observe(AgentEvent::TurnAccepted { turn: start.turn });
        }
        if start.interrupted > 0 {
            observe(AgentEvent::InterruptedCalls(start.interrupted));
        }
        self.drive(session, start, instructions, &stop, steering, observe)
            .await
    }

    async fn drive<F, M: Send>(
        &self,
        session: &Arc<Session>,
        start: TurnStart,
        instructions: String,
        stop: &CancellationToken,
        steering: Option<&SteeringInbox<M>>,
        observe: &mut F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        // Only this running operation knows which reserved calls never received
        // execution authority. Process-loss recovery remains conservatively Unknown.
        let turn = start.turn;
        let (mut assistant_seen_in_turn, mut prefix_bound_continuation) =
            session.turn_replay_state(turn)?;
        let mut unstarted = BTreeSet::new();
        let outcome = async {
            let mut length_recovery_attempted = false;
            let mut replay_rebased = false;
            let mut route_reason = if start.resumed.is_some() {
                ModelRouteReason::Recovery
            } else {
                ModelRouteReason::UserRequest
            };
            loop {
                // Keep a fast in-process model from starving terminal input and
                // cancellation during a long tool sequence.
                tokio::task::yield_now().await;
                if stop.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                if let Some(inbox) = steering {
                    for input in inbox.record_pending(session, turn, self.limits)? {
                        observe(AgentEvent::SteeringCommitted { turn, input });
                    }
                }
                let mut recovered_overflow = false;
                let step = loop {
                    // Opted-in continuation pins executable routes for the Turn.
                    // Discovery can change exposure, not replace this authority.
                    if start.catalog.is_none() {
                        for diagnostic in self.tools.refresh(stop.clone()).await {
                            observe(AgentEvent::ToolCatalogWarning(diagnostic));
                        }
                    }
                    if stop.is_cancelled() {
                        return Err(AgentError::Cancelled);
                    }
                    let prepared = match PreparedRequest::new(
                        session,
                        turn,
                        &self.service,
                        &self.tools,
                        route_reason,
                        self.request_settings(&instructions),
                        start.catalog.as_ref(),
                    ) {
                        Ok(prepared) => prepared,
                        Err(AgentError::ContextTooLarge) if !prefix_bound_continuation => {
                            if self
                                .compact_inner(session, stop, self.keep_bytes(), observe)
                                .await?
                                .is_some()
                            {
                                continue;
                            }
                            return Err(AgentError::ContextTooLarge);
                        }
                        Err(error) => return Err(error),
                    };
                    let output_budget = prepared.output_budget();
                    let mut emitted_output = false;
                    let issued = prepared
                        .issue(session, &self.service, stop, &mut |event| {
                            if matches!(
                                event,
                                AgentEvent::TextDelta(_)
                                    | AgentEvent::ThinkingDelta { .. }
                                    | AgentEvent::ModelOutputObserved
                            ) {
                                emitted_output = true;
                            }
                            observe(event);
                        })
                        .await;
                    let generated = issued.as_ref().map(|step| &step.generated);
                    if !assistant_seen_in_turn
                        && !replay_rebased
                        && matches!(&generated, Err(AgentError::ReplayContextChanged))
                    {
                        session.rebase_provider_replay(turn)?;
                        observe(AgentEvent::ProviderReplayRebased);
                        replay_rebased = true;
                        continue;
                    }
                    let overflow = matches!(
                        &generated,
                        Err(AgentError::Provider(ProviderError {
                            kind: ProviderErrorKind::ContextLength,
                            ..
                        }))
                    ) || matches!(
                        &generated,
                        Ok(GeneratedResponse {
                            response: ModelResponse {
                                termination: ResponseTermination::Incomplete(
                                    IncompleteReason::ContextLength
                                ),
                                ..
                            },
                            ..
                        })
                    );
                    let recoverable_length = !length_recovery_attempted
                        && matches!(
                            &generated,
                            Ok(GeneratedResponse {
                                response: ModelResponse {
                                    termination: ResponseTermination::Incomplete(
                                        IncompleteReason::MaxOutputTokens
                                    ),
                                    usage: ion_ai::Usage {
                                        output_tokens: Some(output),
                                        ..
                                    },
                                    ..
                                },
                                ..
                            }) if *output < u64::from(output_budget)
                        );
                    if ((overflow && !emitted_output) || recoverable_length)
                        && !prefix_bound_continuation
                        && !recovered_overflow
                        && self
                            .compact_inner(session, stop, self.keep_bytes(), observe)
                            .await?
                            .is_some()
                    {
                        recovered_overflow = true;
                        if recoverable_length {
                            length_recovery_attempted = true;
                            observe(AgentEvent::ResponseRestarted);
                        }
                        continue;
                    }
                    break issued?;
                };
                let RequestStep {
                    generated: GeneratedResponse { response, route },
                    prepared,
                    started,
                } = step;
                let tool_catalog = prepared.catalog();
                let execution = ModelExecution {
                    route,
                    returned_model: response.returned_model.clone(),
                };
                let truncated_calls = matches!(
                    response.termination,
                    ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens)
                ) && response
                    .message
                    .content
                    .iter()
                    .any(|part| matches!(part, Content::ToolCall(_)));
                if response.message.role != Role::Assistant
                    || (!matches!(response.termination, ResponseTermination::Completed)
                        && !truncated_calls)
                {
                    return Err(AgentError::IncompleteModelResponse);
                }
                if response
                    .message
                    .provider_replay
                    .as_ref()
                    .is_some_and(|replay| {
                        !replay.is_compatible_with(&execution.route.effective.provider)
                    })
                {
                    return Err(AgentError::InvalidProviderReplay);
                }
                let mut ids = std::collections::HashSet::new();
                let mut calls = Vec::new();
                for content in &response.message.content {
                    if let Content::ToolCall(call) = content {
                        if call.id.is_empty() || call.name.is_empty() || !ids.insert(&call.id) {
                            return Err(AgentError::InvalidToolCall);
                        }
                        calls.push(call.clone());
                    }
                }
                let tool_activities = calls
                    .iter()
                    .map(|call| StoredToolActivity {
                        call_id: call.id.clone(),
                        activity: tool_catalog.activity(call),
                    })
                    .collect::<Vec<_>>();
                prefix_bound_continuation |= response
                    .message
                    .provider_replay
                    .as_ref()
                    .is_some_and(|replay| replay.prefix_bound);
                let committed_content = response.message.content.clone();
                if truncated_calls {
                    assistant_seen_in_turn = true;
                    let results = session.record_truncated_assistant(
                        turn,
                        response.message,
                        tool_activities.clone(),
                        execution.clone(),
                        response.usage,
                    )?;
                    prepared.record_observed_input(session, &execution, response.usage)?;
                    observe(AgentEvent::AssistantCommitted {
                        turn,
                        content: committed_content,
                        tool_activities,
                        termination: response.termination,
                    });
                    for result in results {
                        let activity = calls
                            .iter()
                            .find(|call| call.id == result.call_id)
                            .map_or_else(
                                || ToolActivity::external(&result.name),
                                |call| tool_catalog.activity(call),
                            );
                        observe(AgentEvent::ToolFinished {
                            call_id: result.call_id,
                            name: result.name,
                            activity,
                            outcome: result.outcome,
                        });
                    }
                    continue;
                }
                let final_text = response
                    .message
                    .content
                    .iter()
                    .filter_map(|content| {
                        if let Content::Text(text) = content {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if calls.is_empty() && final_text.trim().is_empty() {
                    return Err(AgentError::IncompleteModelResponse);
                }
                // Steering arriving during a tool stays in the host inbox until
                // the tool result is durable. A final answer and its queued
                // steering enter the Session together or neither does.
                let (complete, committed_steering) = if calls.is_empty()
                    && let Some(inbox) = steering
                {
                    inbox.record_assistant(
                        session,
                        turn,
                        response.message,
                        tool_activities.clone(),
                        execution.clone(),
                        response.usage,
                        self.limits,
                    )?
                } else {
                    (
                        session.record_assistant_with_activities(
                            turn,
                            response.message,
                            tool_activities.clone(),
                            execution.clone(),
                            response.usage,
                            false,
                        )?,
                        Vec::new(),
                    )
                };
                prepared.record_observed_input(session, &execution, response.usage)?;
                observe(AgentEvent::AssistantCommitted {
                    turn,
                    content: committed_content,
                    tool_activities,
                    termination: response.termination,
                });
                for input in committed_steering {
                    observe(AgentEvent::SteeringCommitted { turn, input });
                }
                assistant_seen_in_turn = true;
                if complete {
                    observe(AgentEvent::TurnEnded {
                        turn,
                        reason: TurnEndReason::Completed,
                    });
                    observe(AgentEvent::Final(final_text.clone()));
                    return Ok(final_text);
                }
                route_reason = if calls.is_empty() {
                    ModelRouteReason::Steering
                } else {
                    ModelRouteReason::ToolContinuation
                };
                unstarted.extend(calls.iter().map(|call| call.id.clone()));
                let call_count = calls.len();
                let mut cache_warmer = if call_count == 0 {
                    None
                } else {
                    self.limits.prompt_cache_warming.and_then(|policy| {
                        PromptCacheWarmer::new(
                            policy,
                            prepared.warming_request(),
                            response.usage,
                            started,
                        )
                    })
                };
                let mut activate_tools = BTreeSet::new();
                for (index, call) in calls.into_iter().enumerate() {
                    if stop.is_cancelled() {
                        return Err(AgentError::Cancelled);
                    }
                    let activity = tool_catalog.activity(&call);
                    let occurrence = session.tool_occurrence(turn, &call.id)?;
                    let (invocation, gateway) = crate::ToolInvocation::admitted(
                        &call,
                        stop.clone(),
                        session.id(),
                        turn,
                        occurrence,
                        None,
                        prepared.reasoning(),
                    );
                    let dispatch = tool_catalog.execute_model_call(invocation);
                    let rejection = match &dispatch {
                        ToolDispatch::Rejected(reason) => Some(reason.clone()),
                        _ => None,
                    };
                    let rejected = rejection.is_some();
                    let tool = Box::pin(async {
                        if stop.is_cancelled() {
                            return Err(AgentError::Cancelled);
                        }
                        if !rejected {
                            unstarted.remove(&call.id);
                            observe(AgentEvent::ToolStarted {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                                activity: activity.clone(),
                            });
                        }
                        match dispatch {
                            ToolDispatch::History => Ok(ToolExecution::output(
                                crate::session::history::execute(session, &call, stop.clone())
                                    .await,
                            )),
                            ToolDispatch::Composition(runtime, limits) => crate::code_gateway::run(
                                runtime,
                                limits,
                                session,
                                turn,
                                tool_catalog,
                                &call,
                                prepared.reasoning(),
                                stop,
                                observe,
                            )
                            .await
                            .map(ToolExecution::output),
                            ToolDispatch::Execution(execution) => Ok(execution.await),
                            ToolDispatch::Rejected(reason) => Ok(ToolExecution::output(
                                crate::ToolOutcome::NotDispatched { reason }
                                    .inspection_output()
                                    .into_owned(),
                            )),
                        }
                    });
                    let tool = Box::pin(async { gateway.run(session, stop, tool).await? });
                    let execution = self
                        .execute_tool_with_cache_warming(
                            session,
                            turn,
                            tool,
                            stop,
                            &mut cache_warmer,
                        )
                        .await?;
                    let output = execution.output;
                    activate_tools.extend(execution.activate);
                    let next_context = (index + 1 == call_count && !activate_tools.is_empty())
                        .then(|| ModelContextSnapshot {
                            instructions: instructions.clone(),
                            tools: tool_catalog.declared_specs_with(&activate_tools),
                        });
                    let outcome = if let Some(reason) = rejection {
                        crate::ToolOutcome::NotDispatched { reason }
                    } else {
                        let projection = output.model_projection(
                            self.limits.image_input,
                            self.limits.max_request_bytes,
                        )?;
                        crate::ToolOutcome::Observed { output, projection }
                    };
                    session.record_tool_settlement_with_context(
                        turn,
                        crate::ToolSettlement {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            outcome: outcome.clone(),
                        },
                        next_context,
                    )?;
                    unstarted.remove(&call.id);
                    observe(AgentEvent::ToolFinished {
                        call_id: call.id,
                        name: call.name,
                        activity,
                        outcome,
                    });
                }
            }
        }
        .await;
        match outcome {
            Ok(answer) => Ok(answer),
            Err(error) => {
                // Never disguise failed storage with a second write.
                if !matches!(error, AgentError::Session(_)) {
                    let reason = error.end_reason();
                    for (result, activity) in session.end_turn(turn, reason.clone(), &unstarted)? {
                        observe(AgentEvent::ToolFinished {
                            call_id: result.call_id,
                            name: result.name,
                            activity,
                            outcome: result.outcome,
                        });
                    }
                    observe(AgentEvent::TurnEnded { turn, reason });
                }
                Err(error)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Continuation, not another input acceptance. History must be reconciled.
    TurnResumed {
        point: crate::RecoveryPoint,
    },
    TurnAccepted {
        turn: u64,
    },
    TurnEnded {
        turn: u64,
        reason: TurnEndReason,
    },
    /// Generated content without a human delta. Not call admission or an effect.
    ModelOutputObserved,
    /// Provisional text for the current response only.
    TextDelta(String),
    /// Provisional human thinking, sharing the current response's custody.
    ThinkingDelta {
        block: usize,
        text: String,
    },
    /// Published after the assistant's complete content and call metadata commit.
    AssistantCommitted {
        turn: u64,
        content: Vec<Content>,
        tool_activities: Vec<StoredToolActivity>,
        termination: ResponseTermination,
    },
    /// Published after steering enters the Session, in durable order.
    SteeringCommitted {
        turn: u64,
        input: Message,
    },
    ProviderRetry {
        attempt: usize,
        max_retries: usize,
        delay_ms: u64,
    },
    ContextCompacted {
        through_entry: u64,
    },
    ProviderReplayRebased,
    ProviderReplayNotice {
        action: String,
        reason: String,
        count: usize,
    },
    ResponseRestarted,
    ToolCatalogWarning(String),
    ToolStarted {
        call_id: String,
        name: String,
        arguments: Value,
        activity: ToolActivity,
    },
    /// Published after the typed settlement is durable, including operation closure.
    ToolFinished {
        call_id: String,
        name: String,
        activity: ToolActivity,
        outcome: crate::ToolOutcome,
    },
    ChildToolAdmitted {
        parent_call_id: String,
        intent: crate::ChildIntent,
    },
    ChildToolStarted {
        parent: crate::ToolOccurrence,
        child: usize,
    },
    ChildToolFinished {
        parent: crate::ToolOccurrence,
        child: usize,
        outcome: crate::ChildOutcome,
    },
    InterruptedCalls(usize),
    Final(String),
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("prompt is empty")]
    EmptyPrompt,
    #[error("input is not a valid user message")]
    InvalidUserInput,
    #[error(
        "selected model route does not declare image input; choose an image-capable model or configure the custom route with --images"
    )]
    ImagesUnsupported,
    #[error("turn was cancelled")]
    Cancelled,
    #[error("operation observer panicked; started work has been joined")]
    ObserverPanicked,
    #[error("operation observer panicked; settlement also failed: {0}")]
    ObserverSettlementFailed(#[source] Box<AgentError>),
    #[error("pending input queue is full ({max_encoded_bytes} encoded bytes maximum)")]
    InputQueueFull { max_encoded_bytes: usize },
    #[error("input exceeds route message limit ({max_encoded_bytes} bytes)")]
    InputTooLarge { max_encoded_bytes: usize },
    #[error(
        "model input exceeds the context or request-size limit after available compaction; shorten the current input or, for a context-window limit, select a larger-context model"
    )]
    ContextTooLarge,
    #[error("model returned an invalid context summary: {0}")]
    InvalidSummary(&'static str),
    #[error("model did not return a complete, nonempty assistant response")]
    IncompleteModelResponse,
    #[error("model returned continuation material for another provider")]
    InvalidProviderReplay,
    #[error("provider reasoning prefix changed after replay reset or during tool continuation")]
    ReplayContextChanged,
    #[error("model returned an invalid or duplicate tool call")]
    InvalidToolCall,
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl crate::operation::OperationFailure for AgentError {
    fn expected_cancellation(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl From<crate::OperationError> for AgentError {
    fn from(error: crate::OperationError) -> Self {
        Self::Session(error.into())
    }
}

impl AgentError {
    fn end_reason(&self) -> TurnEndReason {
        match self {
            Self::Cancelled => TurnEndReason::Cancelled,
            Self::Provider(error) => TurnEndReason::Failed(format!("provider: {:?}", error.kind)),
            Self::ContextTooLarge => TurnEndReason::Failed("context_too_large".into()),
            Self::InvalidSummary(reason) => {
                TurnEndReason::Failed(format!("invalid_summary: {reason}"))
            }
            Self::IncompleteModelResponse => {
                TurnEndReason::Failed("incomplete_model_response".into())
            }
            Self::InvalidProviderReplay => TurnEndReason::Failed("invalid_provider_replay".into()),
            Self::ReplayContextChanged => TurnEndReason::Failed("replay_context_changed".into()),
            Self::InvalidToolCall => TurnEndReason::Failed("invalid_tool_call".into()),
            Self::EmptyPrompt
            | Self::InvalidUserInput
            | Self::ImagesUnsupported
            | Self::InputQueueFull { .. }
            | Self::InputTooLarge { .. }
            | Self::ObserverPanicked
            | Self::ObserverSettlementFailed(_)
            | Self::Session(_)
            | Self::Json(_) => TurnEndReason::Failed("agent_error".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_result::ToolOutput;
    use crate::{
        CodingSession, ForkPoint, ToolActivityKind, ToolDefinition, ToolExecutor, ToolExposure,
        ToolPresentation, ToolRegistration,
    };
    use ion_ai::{
        BoxFuture, ImageContent, Message, ModelResponse, ModelStreamEvent, Script,
        ScriptedModelService, ToolCall, ToolSpec, Usage,
    };

    fn tiny_image() -> ImageContent {
        ImageContent::from_bytes(&[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ])
        .unwrap()
    }

    /// Minimal concrete host used only to exercise the core Turn/tool protocol.
    /// Native filesystem/process edge cases belong to ion-host::LocalTools tests.
    struct TestTools {
        root: std::path::PathBuf,
    }

    impl TestTools {
        fn new(root: impl AsRef<std::path::Path>) -> Self {
            Self {
                root: root.as_ref().canonicalize().unwrap(),
            }
        }
    }

    impl ToolSource for TestTools {
        fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
            let definitions = [
            ("read", ToolActivityKind::Read, "path", serde_json::json!({
                "type":"object", "required":["path"], "properties":{"path":{"type":"string"}}
            })),
            ("write", ToolActivityKind::Write, "path", serde_json::json!({
                "type":"object", "required":["path","content"],
                "properties":{"path":{"type":"string"},"content":{"type":"string"}}
            })),
            ("exec", ToolActivityKind::Command, "command", serde_json::json!({
                "type":"object", "required":["command"], "properties":{"command":{"type":"string"}}
            })),
        ].map(|(name, kind, key, input_schema)| ToolDefinition {
            spec: ToolSpec { name: name.into(), description: format!("Fixture {name}"), input_schema },
            presentation: ToolPresentation::argument(kind, key),
            exposure: ToolExposure::Direct,
        });
            definitions
                .into_iter()
                .map(|definition| ToolRegistration::new(definition, self.clone()))
                .collect()
        }
    }

    impl ToolExecutor for TestTools {
        fn execute<'a>(
            &'a self,
            invocation: crate::ToolInvocation<'a>,
        ) -> BoxFuture<'a, ToolOutput> {
            let call = invocation.call();
            let stop = invocation.stop().clone();
            Box::pin(async move {
                if stop.is_cancelled() {
                    return ToolOutput {
                        value: serde_json::json!({"error":"cancelled before tool start"}),
                        images: Vec::new(),
                        is_error: true,
                    };
                }
                match call.name.as_str() {
                    "read" => {
                        let Some(path) = call.arguments.get("path").and_then(Value::as_str) else {
                            return ToolOutput {
                                value: serde_json::json!({"error":"invalid read arguments"}),
                                images: Vec::new(),
                                is_error: true,
                            };
                        };
                        match std::fs::read_to_string(self.root.join(path)) {
                            Ok(content) => ToolOutput {
                                value: serde_json::json!({"path":path,"content":content}),
                                images: Vec::new(),
                                is_error: false,
                            },
                            Err(error) => ToolOutput {
                                value: serde_json::json!({"error":error.to_string()}),
                                images: Vec::new(),
                                is_error: true,
                            },
                        }
                    }
                    "write" => {
                        let path = call.arguments.get("path").and_then(Value::as_str);
                        let content = call.arguments.get("content").and_then(Value::as_str);
                        let (Some(path), Some(content)) = (path, content) else {
                            return ToolOutput {
                                value: serde_json::json!({"error":"invalid write arguments"}),
                                images: Vec::new(),
                                is_error: true,
                            };
                        };
                        let path = self.root.join(path);
                        if let Some(parent) = path.parent()
                            && let Err(error) = std::fs::create_dir_all(parent)
                        {
                            return ToolOutput {
                                value: serde_json::json!({"error":error.to_string()}),
                                images: Vec::new(),
                                is_error: true,
                            };
                        }
                        match std::fs::write(&path, content) {
                            Ok(()) => ToolOutput {
                                value: serde_json::json!({"path":path.to_string_lossy(),"written":content.len()}),
                                images: Vec::new(),
                                is_error: false,
                            },
                            Err(error) => ToolOutput {
                                value: serde_json::json!({"error":error.to_string()}),
                                images: Vec::new(),
                                is_error: true,
                            },
                        }
                    }
                    "exec" => ToolOutput {
                        value: serde_json::json!({"command":call.arguments.get("command"),"exit_code":0}),
                        images: Vec::new(),
                        is_error: false,
                    },
                    _ => ToolOutput {
                        value: serde_json::json!({"error":format!("unknown tool: {}",call.name)}),
                        images: Vec::new(),
                        is_error: true,
                    },
                }
            })
        }
    }

    #[test]
    fn coding_output_budget_uses_model_ceiling_and_remaining_context() {
        let limits = AgentLimits {
            max_output_tokens: 128_000,
            context_window_tokens: Some(200_000),
            ..AgentLimits::default()
        };
        assert_eq!(limits.output_budget(900, 300, 128_000), Some(128_000));
        assert_eq!(
            limits.output_budget(300_000, 100_000, 128_000),
            Some(91_808)
        );
        assert_eq!(limits.output_budget(600_000, 200_000, 128_000), None);
        assert_eq!(limits.output_budget(9 * 1024 * 1024, 300, 128_000), None);
    }

    #[tokio::test]
    async fn coding_requests_send_the_context_clamped_model_ceiling() {
        let root = std::env::temp_dir().join(format!("ion-budget-run-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("first".into())]),
            response(vec![Content::Text("second".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model())
            .with_limits(AgentLimits {
                max_output_tokens: 128_000,
                context_window_tokens: Some(200_000),
                ..AgentLimits::default()
            });
        agent
            .submit(
                session.clone(),
                "short".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        agent
            .submit(
                session.clone(),
                "another".into(),
                "x".repeat(300_000),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let requests = scripts.requests();
        assert_eq!(requests[0].controls.max_output_tokens, 128_000);
        assert!((90_000..92_000).contains(&requests[1].controls.max_output_tokens));

        // Compare the borrowed sizing view with the original encoded neutral
        // request, including escaped text, both image locations and a large
        // historical timeline that must not consume the current prompt budget.
        let mut request = requests[1].clone();
        request.instructions = Some("quoted \"\\text\n🦀\0".into());
        let image = tiny_image();
        request.messages.push(Message {
            role: Role::User,
            content: vec![Content::Text("look".into()), Content::Image(image.clone())],
            provider_replay: None,
        });
        request.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ion_ai::ToolResult {
                call_id: "image".into(),
                name: "read".into(),
                result: serde_json::json!({"note":"quoted \" 🦀"}),
                images: vec![image.clone()],
                is_error: false,
            })],
            provider_replay: None,
        });
        request.messages[1].provider_replay = Some(ion_ai::ProviderReplay::new(
            model().provider,
            "opaque",
            serde_json::json!("x".repeat(32_768)),
        ));
        let mut reference = request.clone();
        reference.context_timeline = None;
        // The representation limit includes retained replay; the neutral token
        // estimate does not charge its duplicate payload or opaque metadata.
        let bytes = serde_json::to_vec(&reference).unwrap().len();
        for message in &mut reference.messages {
            message.provider_replay = None;
        }
        let neutral_bytes = serde_json::to_vec(&reference).unwrap().len();
        let input = neutral_bytes
            .saturating_sub(2 * image.data().len())
            .div_ceil(3) as u64
            + 2 * 16_384;
        let limits = AgentLimits {
            max_request_bytes: bytes,
            context_window_tokens: Some(50_000),
            ..agent.limits
        };
        assert_eq!(
            limits.request_output_budget(&request, 128_000).unwrap(),
            limits.output_budget(bytes, input, 128_000),
        );
        assert!(limits.output_budget(bytes, input, 128_000).is_some());
        assert_eq!(
            AgentLimits {
                max_request_bytes: bytes - 1,
                ..limits
            }
            .request_output_budget(&request, 128_000)
            .unwrap(),
            None,
        );
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn response(content: Vec<Content>) -> Script {
        Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
            message: Message {
                role: Role::Assistant,
                content,
                provider_replay: None,
            },
            usage: Usage::unknown(),
            termination: ResponseTermination::Completed,
            returned_model: Some("test".into()),
        })])
    }
    fn model() -> ModelRef {
        ModelRef {
            provider: "test".into(),
            model: "test".into(),
        }
    }

    #[tokio::test]
    async fn request_admission_failure_does_not_issue_or_publish_partial_boundary() {
        let root = std::env::temp_dir().join(format!("ion-admission-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("first answer".into())]),
            response(vec![Content::Text("must not issue".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model());
        agent
            .submit(
                session.clone(),
                "first".into(),
                "first context".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let before = session.view().unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TRIGGER reject_context BEFORE INSERT ON entries WHEN json_extract(NEW.body, '$.kind') = 'model_context_changed' BEGIN SELECT RAISE(ABORT, 'context unavailable'); END;").unwrap();
        let changed = ModelRef {
            model: "changed".into(),
            ..model()
        };
        let agent = Agent::new(
            scripts.clone(),
            Arc::new(TestTools::new(&root)),
            changed.clone(),
        );
        let result = agent
            .submit(
                session.clone(),
                "second".into(),
                "second context".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await;
        assert!(matches!(result, Err(AgentError::Session(_))), "{result:?}");
        assert_eq!(
            scripts.requests().len(),
            1,
            "failed admission issued a provider request"
        );
        let after = session.view().unwrap();
        assert_eq!(after.last_model, Some(changed)); // User acceptance is a separate durable boundary.
        assert_eq!(after.last_context, before.last_context);
        assert_eq!(after.last_effective_model, before.last_effective_model);
        assert!(
            !after
                .entries
                .iter()
                .any(|entry| matches!(entry, crate::SessionEntry::EffectiveModelChanged { .. }))
        );
        drop(session);
        let reopened = Arc::new(CodingSession::open(&path).unwrap());
        assert_eq!(
            reopened.view().unwrap().last_effective_model,
            before.last_effective_model
        );
        assert_eq!(reopened.model_context().unwrap(), before.last_context);
        drop(reopened);
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn issued_request_keeps_capability_and_metadata_until_its_calls_settle() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct ChangingSource(AtomicUsize);
        struct BoundExecutor(usize);
        impl ToolExecutor for BoundExecutor {
            fn execute<'a>(
                &'a self,
                _invocation: crate::ToolInvocation<'a>,
            ) -> BoxFuture<'a, ToolOutput> {
                Box::pin(async move {
                    ToolOutput {
                        value: serde_json::json!({"generation": self.0}),
                        images: Vec::new(),
                        is_error: false,
                    }
                })
            }
        }
        impl ToolSource for ChangingSource {
            fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
                let version = self.0.load(Ordering::SeqCst);
                vec![ToolRegistration::new(
                    ToolDefinition {
                        spec: ToolSpec {
                            name: "probe".into(),
                            description: format!("generation {version}"),
                            input_schema: serde_json::json!({"type":"object"}),
                        },
                        presentation: ToolPresentation::static_target(
                            crate::ToolActivityKind::Read,
                            format!("generation {version}"),
                        ),
                        exposure: crate::ToolExposure::Direct,
                    },
                    Arc::new(BoundExecutor(version)),
                )]
            }
        }
        let root = std::env::temp_dir().join(format!("ion-issued-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let call = |id: &str| {
            Content::ToolCall(ToolCall {
                id: id.into(),
                name: "probe".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
            })
        };
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![call("first")]),
            response(vec![call("second")]),
            response(vec![Content::Text("done".into())]),
        ]));
        let source = Arc::new(ChangingSource(AtomicUsize::new(0)));
        let agent = Agent::new(scripts.clone(), source.clone(), model());
        agent
            .submit(
                session.clone(),
                "probe".into(),
                "context".into(),
                CancellationToken::new(),
                move |event| {
                    if matches!(event, AgentEvent::AssistantCommitted { .. }) {
                        source.0.store(1, Ordering::SeqCst);
                    }
                },
            )
            .await
            .unwrap();
        let view = session.view().unwrap();
        let generations = view
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|part| match part {
                Content::ToolResult(result) => result.result["generation"].as_u64(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(generations, [0, 1]);
        let subjects = view
            .entries
            .iter()
            .filter_map(|entry| match entry {
                crate::SessionEntry::Assistant {
                    tool_activities, ..
                } => tool_activities
                    .first()
                    .and_then(|activity| activity.activity.subject.as_deref()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(subjects, ["generation 0", "generation 1"]);
        let requests = scripts.requests();
        assert_eq!(requests[0].tools[0].description, "generation 0");
        assert_eq!(requests[1].tools[0].description, "generation 1");
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    struct SlowRead;

    impl ToolSource for SlowRead {
        fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
            vec![ToolRegistration::new(
                ToolDefinition {
                    spec: ToolSpec {
                        name: "read".into(),
                        description: "slow read".into(),
                        input_schema: serde_json::json!({
                            "type":"object",
                            "additionalProperties":false,
                            "properties":{}
                        }),
                    },
                    presentation: ToolPresentation::argument(ToolActivityKind::Read, "path"),
                    exposure: ToolExposure::Direct,
                },
                self.clone(),
            )]
        }
    }

    impl ToolExecutor for SlowRead {
        fn execute<'a>(
            &'a self,
            _invocation: crate::ToolInvocation<'a>,
        ) -> BoxFuture<'a, ToolOutput> {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                ToolOutput {
                    value: serde_json::json!({"content":"ok"}),
                    images: Vec::new(),
                    is_error: false,
                }
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn streaming_cache_warm_replays_request_without_entering_model_context() {
        let root = std::env::temp_dir().join(format!("ion-cache-warm-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let first = Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "read-1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            usage: Usage::known_with_cache(100_000, 4, 100_000, 0),
            termination: ResponseTermination::Completed,
            returned_model: Some("test".into()),
        })]);
        let warm = Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::Text("ignored refresh output".into())],
                provider_replay: None,
            },
            usage: Usage::known(100_000, 1),
            termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
            returned_model: Some("test".into()),
        })]);
        let service = Arc::new(ScriptedModelService::new([
            first,
            warm,
            response(vec![Content::Text("done".into())]),
        ]));
        let agent =
            Agent::new(service.clone(), Arc::new(SlowRead), model()).with_limits(AgentLimits {
                prompt_cache_warming: Some(PromptCacheWarmingPolicy {
                    // The shortest lifetime with a usable refresh point; the
                    // warm entry lands one second into the tool call.
                    lifetime_seconds: 11,
                    cache_write_microusd_per_million: 5_000_000,
                    cache_read_microusd_per_million: 200_000,
                    output_microusd_per_million: 20_000_000,
                    minimum_savings_microusd: 0,
                }),
                ..AgentLimits::default()
            });

        assert_eq!(
            agent
                .submit(
                    session.clone(),
                    "inspect".into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap(),
            "done"
        );

        let requests = service.requests();
        assert_eq!(requests.len(), 3);
        let provider_id = session.provider_session_id().to_string();
        assert!(requests.iter().all(|request| {
            request.provider_session_id.as_deref() == Some(provider_id.as_str())
        }));
        assert_eq!(requests[0].route.reason, ModelRouteReason::UserRequest);
        assert_eq!(requests[1].route.reason, ModelRouteReason::Auxiliary);
        assert_eq!(requests[1].controls.max_output_tokens, 1);
        assert_eq!(requests[2].route.reason, ModelRouteReason::ToolContinuation);
        assert_eq!(requests[2].messages.len(), 3);
        assert!(session.view().unwrap().entries.iter().any(|entry| matches!(
            entry,
            crate::session::SessionEntry::CacheWarm {
                execution: ModelExecution {
                    route: ModelRoute {
                        reason: ModelRouteReason::Auxiliary,
                        ..
                    },
                    returned_model: Some(returned),
                },
                usage: Usage {
                    output_tokens: Some(1),
                    ..
                },
                ..
            } if returned == "test"
        )));
        assert_eq!(session.context_messages().unwrap().len(), 4);

        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    struct DelayedInitialResponse {
        inner: Arc<ScriptedModelService>,
        delay: std::time::Duration,
    }

    impl ModelService for DelayedInitialResponse {
        fn stream<'a>(
            &'a self,
            request: ModelRequest,
        ) -> BoxFuture<'a, Result<ion_ai::ModelStream, ion_ai::ProviderError>> {
            Box::pin(async move {
                if self.inner.requests().is_empty() {
                    tokio::time::sleep(self.delay).await;
                }
                self.inner.stream(request).await
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cache_warming_requires_a_usable_refresh_point() {
        // A deadline must be positive and inside the active horizon. Neither
        // the cache lifetime nor that horizon may elapse awaiting the response.
        for (lifetime_seconds, delay_seconds) in [
            (0, 0),
            (10, 0),
            (4_000, 0),
            (u64::MAX, 0),
            (300, 300),
            (3_999, 3_600),
        ] {
            let root = std::env::temp_dir().join(format!(
                "ion-cache-short-{lifetime_seconds}-{}",
                uuid::Uuid::now_v7()
            ));
            std::fs::create_dir(&root).unwrap();
            let session =
                Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
            let service = Arc::new(ScriptedModelService::new([
                Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Content::ToolCall(ToolCall {
                            id: "read-1".into(),
                            name: "read".into(),
                            arguments: serde_json::json!({}),
                            raw_arguments: None,
                        })],
                        provider_replay: None,
                    },
                    usage: Usage::known_with_cache(100_000, 4, 100_000, 0),
                    termination: ResponseTermination::Completed,
                    returned_model: Some("test".into()),
                })]),
                // Baseline ignores the lifetime and refreshes at once, consuming
                // the answer script as its refresh reply; the spare keeps that
                // continuation valid so the pre-fix signal is the request count
                // below, not script exhaustion or a different answer.
                response(vec![Content::Text("done".into())]),
                response(vec![Content::Text("ignored refresh output".into())]),
            ]));
            let delayed = Arc::new(DelayedInitialResponse {
                inner: service.clone(),
                delay: std::time::Duration::from_secs(delay_seconds),
            });
            let agent = Agent::new(delayed, Arc::new(SlowRead), model()).with_limits(AgentLimits {
                prompt_cache_warming: Some(PromptCacheWarmingPolicy {
                    lifetime_seconds,
                    cache_write_microusd_per_million: 5_000_000,
                    cache_read_microusd_per_million: 200_000,
                    output_microusd_per_million: 20_000_000,
                    minimum_savings_microusd: 0,
                }),
                ..AgentLimits::default()
            });
            let result = agent
                .submit(
                    session.clone(),
                    "inspect".into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await;

            let requests = service.requests();
            assert_eq!(
                requests.len(),
                2,
                "lifetime {lifetime_seconds}s after a {delay_seconds}s response scheduled a refresh"
            );
            assert!(
                requests
                    .iter()
                    .all(|request| request.route.reason != ModelRouteReason::Auxiliary)
            );
            let entries = session.view().unwrap().entries;
            assert!(
                !entries
                    .iter()
                    .any(|entry| matches!(entry, crate::session::SessionEntry::CacheWarm { .. }))
            );
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| matches!(
                        entry,
                        crate::session::SessionEntry::ToolResult { .. }
                    ))
                    .count(),
                1
            );
            assert_eq!(result.unwrap(), "done");
            drop(session);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn transient_provider_error_retries_only_before_stream_output() {
        let root = std::env::temp_dir().join(format!("ion-retry-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let service = Arc::new(ScriptedModelService::new([
            Script::OpenError(ProviderError {
                kind: ProviderErrorKind::Overloaded,
                message: "busy".into(),
                retry_after_ms: Some(10),
            }),
            response(vec![Content::Text("done".into())]),
        ]));
        let agent = Agent::new(service.clone(), Arc::new(TestTools::new(&root)), model());
        let request = ModelRequest {
            route: ModelRoute::direct(model(), ModelRouteReason::UserRequest),
            provider_session_id: Some(uuid::Uuid::now_v7().to_string()),
            instructions: None,
            messages: Vec::new(),
            tools: Vec::new(),
            context_timeline: None,
            prompt_cache: ion_ai::PromptCacheIntent::Default,
            controls: GenerationControls {
                max_output_tokens: 128,
                temperature: None,
                top_p: None,
                reasoning: Reasoning::ProviderDefault,
                tool_choice: ToolChoice::None,
                parallel_tool_calls: false,
            },
        };
        let mut retry_delay = None;
        let generated = generate_with_retry(
            &agent.service,
            &request,
            &CancellationToken::new(),
            &mut |event| {
                if let AgentEvent::ProviderRetry { delay_ms, .. } = event {
                    retry_delay = Some(delay_ms);
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(retry_delay, Some(10));
        assert_eq!(generated.route.reason, ModelRouteReason::Retry);
        let mut retry_request = request.clone();
        retry_request.route.reason = ModelRouteReason::Retry;
        assert_eq!(service.requests(), vec![request.clone(), retry_request]);
        assert_eq!(service.requests().len(), 2);

        let service = Arc::new(ScriptedModelService::new([
            Script::Stream(vec![ModelStreamEvent::TextDelta("partial".into())]),
            response(vec![Content::Text("should not be used".into())]),
        ]));
        let agent = Agent::new(service.clone(), Arc::new(TestTools::new(&root)), model());
        let mut visible = String::new();
        let error = generate_with_retry(
            &agent.service,
            &request,
            &CancellationToken::new(),
            &mut |event| {
                if let AgentEvent::TextDelta(text) = event {
                    visible.push_str(&text);
                }
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AgentError::IncompleteModelResponse));
        assert_eq!(visible, "partial");
        assert_eq!(service.requests().len(), 1);

        let service = Arc::new(ScriptedModelService::new([
            Script::OpenError(ProviderError {
                kind: ProviderErrorKind::RateLimited,
                message: "retry later".into(),
                retry_after_ms: Some(60_001),
            }),
            response(vec![Content::Text("should not be used".into())]),
        ]));
        let agent = Agent::new(service.clone(), Arc::new(TestTools::new(&root)), model());
        let error = generate_with_retry(
            &agent.service,
            &request,
            &CancellationToken::new(),
            &mut |_| {},
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AgentError::Provider(_)));
        assert_eq!(service.requests().len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn changed_signed_context_rebases_once_before_dispatch() {
        let root = std::env::temp_dir().join(format!("ion-rebase-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let (prior, _) = session.begin_turn("first".into(), model()).unwrap();
        session
            .record_assistant(
                prior,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("prior".into())],
                    provider_replay: Some(
                        ion_ai::ProviderReplay::new("test", "opaque", serde_json::json!("signed"))
                            .with_prefix_binding(true),
                    ),
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        let service = Arc::new(ScriptedModelService::new([
            Script::OpenError(ProviderError {
                kind: ProviderErrorKind::ReplayContextChanged,
                message: "changed".into(),
                retry_after_ms: None,
            }),
            response(vec![Content::Text("done".into())]),
        ]));
        let agent = Agent::new(service.clone(), Arc::new(TestTools::new(&root)), model());
        let rebases = Arc::new(Mutex::new(0));
        let observed_rebases = rebases.clone();
        assert_eq!(
            agent
                .submit(
                    session.clone(),
                    "second".into(),
                    "instructions".into(),
                    CancellationToken::new(),
                    move |event| {
                        if matches!(event, AgentEvent::ProviderReplayRebased) {
                            *observed_rebases.lock().unwrap() += 1;
                        }
                    },
                )
                .await
                .unwrap(),
            "done"
        );
        assert_eq!(*rebases.lock().unwrap(), 1);
        let requests = service.requests();
        assert!(requests[0].messages[1].provider_replay.is_some());
        assert!(requests[1].messages[1].provider_replay.is_none());
        assert!(session.messages().unwrap()[1].provider_replay.is_some());
        assert!(
            session.context_messages().unwrap()[1]
                .provider_replay
                .is_none()
        );
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn signed_tool_continuation_counts_neutral_content_once() {
        let root = std::env::temp_dir().join(format!("ion-replay-budget-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file.txt"), "content").unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let content = vec![
            Content::Text("x".repeat(5_000)),
            Content::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path":"file.txt"}),
                raw_arguments: None,
            }),
        ];
        let replay = ion_ai::ProviderReplay::new(
            model().provider,
            "opaque",
            serde_json::to_value(&content).unwrap(),
        )
        .with_prefix_binding(true);
        let service = Arc::new(ScriptedModelService::new([
            Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content,
                    provider_replay: Some(replay.clone()),
                },
                usage: Usage::unknown(),
                termination: ResponseTermination::Completed,
                returned_model: Some("test".into()),
            })]),
            response(vec![Content::Text("done".into())]),
        ]));
        let agent = Agent::new(service.clone(), Arc::new(TestTools::new(&root)), model())
            .with_limits(AgentLimits {
                context_window_tokens: Some(11_800),
                max_output_tokens: 256,
                ..AgentLimits::default()
            });
        let result = agent
            .submit(
                session.clone(),
                "read file".into(),
                "instructions".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await;
        let requests = service.requests();
        let view = session.view().unwrap();
        drop((agent, session));
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(result.unwrap(), "done");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].controls.max_output_tokens, 256);
        assert_eq!(
            requests[1].messages[1].provider_replay.as_ref(),
            Some(&replay)
        );
        assert!(!view.entries.iter().any(|entry| matches!(
            entry,
            crate::SessionEntry::Compacted { .. }
                | crate::SessionEntry::ProviderReplayRebased { .. }
        )));
    }

    #[tokio::test]
    async fn signed_tool_continuation_does_not_rebase_or_repeat_tool() {
        let root = std::env::temp_dir().join(format!("ion-tool-prefix-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file.txt"), "content").unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let service = Arc::new(ScriptedModelService::new([
            Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(ToolCall {
                        id: "call_1".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path":"file.txt"}),
                        raw_arguments: None,
                    })],
                    provider_replay: Some(
                        ion_ai::ProviderReplay::new("test", "opaque", serde_json::json!("signed"))
                            .with_prefix_binding(true),
                    ),
                },
                usage: Usage::unknown(),
                termination: ResponseTermination::Completed,
                returned_model: Some("test".into()),
            })]),
            Script::OpenError(ProviderError {
                kind: ProviderErrorKind::ReplayContextChanged,
                message: "changed".into(),
                retry_after_ms: None,
            }),
        ]));
        let agent = Agent::new(service.clone(), Arc::new(TestTools::new(&root)), model());
        let error = agent
            .submit(
                session.clone(),
                "read file".into(),
                "instructions".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::ReplayContextChanged));
        assert_eq!(service.requests().len(), 2);
        let view = session.view().unwrap();
        assert_eq!(
            view.entries
                .iter()
                .filter(|entry| matches!(entry, crate::session::SessionEntry::ToolResult { .. }))
                .count(),
            1
        );
        assert!(!view.entries.iter().any(|entry| matches!(
            entry,
            crate::session::SessionEntry::ProviderReplayRebased { .. }
        )));
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn coding_task_uses_one_loop_and_resumes_after_reopen() {
        let root = std::env::temp_dir().join(format!("ion-agent-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![
                Content::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "write".into(),
                    arguments: serde_json::json!({"path":"answer.txt","content":"hello ion\n"}),
                    raw_arguments: None,
                }),
                Content::ToolCall(ToolCall {
                    id: "call-2".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path":"answer.txt"}),
                    raw_arguments: None,
                }),
            ]),
            response(vec![Content::Text("created and checked".into())]),
        ]));
        let tools = Arc::new(TestTools::new(&root));
        let agent = Agent::new(scripts, tools.clone(), model());
        let answer = agent
            .submit(
                session.clone(),
                "create the file".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(answer, "created and checked");
        assert_eq!(
            std::fs::read_to_string(root.join("answer.txt")).unwrap(),
            "hello ion\n"
        );
        assert_eq!(session.messages().unwrap().len(), 5);
        drop(session);
        let reopened = Arc::new(CodingSession::open(&path).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([response(vec![Content::Text(
            "still here".into(),
        )])]));
        let agent = Agent::new(scripts, tools, model());
        let answer = agent
            .submit(
                reopened.clone(),
                "continue".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(answer, "still here");
        assert_eq!(reopened.messages().unwrap().len(), 7);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn deferred_tool_search_activates_next_request_and_survives_reopen_and_fork() {
        struct DeferredTool;

        impl ToolSource for DeferredTool {
            fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
                let definitions = {
                    vec![
                        ToolDefinition::external(ToolSpec {
                            name: "special_lookup".into(),
                            description: "Look up specialized project metadata".into(),
                            input_schema: serde_json::json!({
                                "type":"object",
                                "additionalProperties":false,
                                "properties":{}
                            }),
                        })
                        .deferred(),
                    ]
                };
                definitions
                    .into_iter()
                    .map(|definition| ToolRegistration::new(definition, self.clone()))
                    .collect()
            }
        }

        impl ToolExecutor for DeferredTool {
            fn execute<'a>(
                &'a self,
                invocation: crate::ToolInvocation<'a>,
            ) -> BoxFuture<'a, ToolOutput> {
                let call = invocation.call();
                Box::pin(async move {
                    ToolOutput {
                        value: serde_json::json!({"tool":call.name,"value":"found"}),
                        images: Vec::new(),
                        is_error: false,
                    }
                })
            }
        }

        for scoped in [false, true] {
            let root =
                std::env::temp_dir().join(format!("ion-deferred-tool-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&root).unwrap();
            let path = root.join("session.sqlite");
            let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
            let scripts = Arc::new(ScriptedModelService::new([
                response(vec![Content::ToolCall(ToolCall {
                    id: "search".into(),
                    name: "tool_search".into(),
                    arguments: serde_json::json!({"query":"specialized metadata"}),
                    raw_arguments: None,
                })]),
                response(vec![Content::ToolCall(ToolCall {
                    id: "use-special".into(),
                    name: "special_lookup".into(),
                    arguments: serde_json::json!({}),
                    raw_arguments: None,
                })]),
                response(vec![Content::Text("done".into())]),
            ]));
            let agent = Agent::new(scripts.clone(), Arc::new(DeferredTool), model());
            let agent = if scoped {
                agent.with_recovery_scope(
                    crate::AuthorityScope::new("deferred-app/grants-v1").unwrap(),
                )
            } else {
                agent
            };
            assert_eq!(
                agent
                    .submit(
                        session.clone(),
                        "find the metadata".into(),
                        "test".into(),
                        CancellationToken::new(),
                        |_| {},
                    )
                    .await
                    .unwrap(),
                "done"
            );

            let requests = scripts.requests();
            assert_eq!(
                requests[0]
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                ["tool_search"]
            );
            assert_eq!(
                requests[1]
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                ["special_lookup"]
            );
            let timeline = requests[1].context_timeline.as_ref().unwrap();
            assert_eq!(
                timeline
                    .initial
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                ["tool_search"]
            );
            assert_eq!(timeline.changes.len(), 1);
            assert_eq!(timeline.changes[0].after_message, 3);
            assert_eq!(
                timeline.changes[0]
                    .context
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                ["special_lookup"]
            );
            assert_eq!(
                requests[2]
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                ["special_lookup"]
            );
            assert!(session.view().unwrap().entries.iter().any(|entry| matches!(
                entry,
                crate::session::SessionEntry::ModelContextChanged { context, .. }
                    if context.tools.iter().any(|tool| tool.name == "special_lookup")
            )));
            drop(session);

            let reopened = Arc::new(CodingSession::open(&path).unwrap());
            assert_eq!(
                reopened
                    .model_context()
                    .unwrap()
                    .unwrap()
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                ["special_lookup"]
            );
            let fork_path = root.join("fork.sqlite");
            let fork = Arc::new(
                reopened
                    .fork_to(&fork_path, ForkPoint::AfterTurn(1))
                    .unwrap(),
            );

            let resumed_scripts =
                Arc::new(ScriptedModelService::new([response(vec![Content::Text(
                    "resumed".into(),
                )])]));
            let resumed_agent =
                Agent::new(resumed_scripts.clone(), Arc::new(DeferredTool), model());
            assert_eq!(
                resumed_agent
                    .submit(
                        reopened.clone(),
                        "continue after reopen".into(),
                        "test".into(),
                        CancellationToken::new(),
                        |_| {},
                    )
                    .await
                    .unwrap(),
                "resumed"
            );
            assert_eq!(
                resumed_scripts.requests()[0]
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                ["special_lookup"]
            );

            let fork_scripts =
                Arc::new(ScriptedModelService::new([response(vec![Content::Text(
                    "forked".into(),
                )])]));
            let fork_agent = Agent::new(fork_scripts.clone(), Arc::new(DeferredTool), model());
            assert_eq!(
                fork_agent
                    .submit(
                        fork.clone(),
                        "continue from fork".into(),
                        "test".into(),
                        CancellationToken::new(),
                        |_| {},
                    )
                    .await
                    .unwrap(),
                "forked"
            );
            assert_eq!(
                fork_scripts.requests()[0]
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                ["special_lookup"]
            );
            drop(fork);
            drop(reopened);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn model_result_limits_preserve_observed_effects_and_inspection() {
        struct ReportingTool {
            root: std::path::PathBuf,
            output: ToolOutput,
        }
        impl ToolSource for ReportingTool {
            fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
                vec![ToolRegistration::new(
                    ToolDefinition::external(ToolSpec {
                        name: "report".into(),
                        description: "Write a marker and return a report".into(),
                        input_schema: serde_json::json!({"type":"object"}),
                    }),
                    self,
                )]
            }
        }
        impl ToolExecutor for ReportingTool {
            fn execute<'a>(
                &'a self,
                _invocation: crate::ToolInvocation<'a>,
            ) -> BoxFuture<'a, ToolOutput> {
                Box::pin(async move {
                    std::fs::write(self.root.join("mutation.txt"), "effect observed").unwrap();
                    self.output.clone()
                })
            }
        }
        for output in [
            ToolOutput {
                value: serde_json::json!({"report":"x".repeat(32_768)}),
                images: Vec::new(),
                is_error: false,
            },
            ToolOutput {
                value: serde_json::json!({"path":"picture.png"}),
                images: vec![tiny_image()],
                is_error: false,
            },
            ToolOutput {
                value: serde_json::json!({"report":"x".repeat(32_768)}),
                images: Vec::new(),
                is_error: true,
            },
            ToolOutput {
                value: serde_json::json!({"path":"picture.png"}),
                images: vec![tiny_image()],
                is_error: true,
            },
        ] {
            let bytes = serde_json::to_vec(&(&output.value, &output.images))
                .unwrap()
                .len();
            assert_eq!(
                output.model_projection(true, bytes).unwrap(),
                crate::ToolResultProjection::Observed,
            );
            assert_eq!(
                output.model_projection(true, bytes - 1).unwrap(),
                crate::ToolResultProjection::RequestLimitExceeded,
            );
            let root =
                std::env::temp_dir().join(format!("ion-result-views-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&root).unwrap();
            let path = root.join("session.sqlite");
            let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
            let scripts = Arc::new(ScriptedModelService::new([
                response(vec![Content::ToolCall(ToolCall {
                    id: "report-call".into(),
                    name: "report".into(),
                    arguments: serde_json::json!({}),
                    raw_arguments: None,
                })]),
                response(vec![Content::Text("done".into())]),
                response(vec![Content::Text("summary".into())]),
            ]));
            let agent = Agent::new(
                scripts.clone(),
                Arc::new(ReportingTool {
                    root: root.clone(),
                    output: output.clone(),
                }),
                model(),
            )
            .with_limits(AgentLimits {
                max_request_bytes: 4_096,
                ..AgentLimits::default()
            });
            let live = Arc::new(Mutex::new(crate::LiveTranscript::with_user_input(
                &Message {
                    role: Role::User,
                    content: vec![Content::Text("report".into())],
                    provider_replay: None,
                },
            )));
            let observed_live = live.clone();
            agent
                .submit(
                    session.clone(),
                    "report".into(),
                    "test".into(),
                    CancellationToken::new(),
                    move |event| observed_live.lock().unwrap().observe(&event),
                )
                .await
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(root.join("mutation.txt")).unwrap(),
                "effect observed"
            );
            let requests = scripts.requests();
            let Content::ToolResult(model_result) = &requests[1].messages[2].content[0] else {
                panic!("missing projected result")
            };
            assert_eq!(model_result.is_error, output.is_error);
            assert!(model_result.images.is_empty());
            assert_eq!(
                model_result.result["output_withheld"],
                serde_json::to_value(output.model_projection(false, 4_096).unwrap()).unwrap()
            );
            assert!(model_result.result.get("notice").is_some());
            assert!(model_result.result.get("error").is_none());
            let view = session.view().unwrap();
            let (result_entry, observed) = view
                .entries
                .iter()
                .enumerate()
                .find_map(|(index, entry)| match entry {
                    crate::SessionEntry::ToolResult { result, .. } => {
                        Some((index as u64 + 1, result))
                    }
                    _ => None,
                })
                .unwrap();
            assert!(
                model_result.result["notice"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("Committed Session entry {result_entry}")),
                "withheld notice lacks its stable entry locator: {}",
                model_result.result
            );
            let crate::ToolOutcome::Observed {
                output: observed, ..
            } = &observed.outcome
            else {
                panic!("missing observed host output")
            };
            assert_eq!(
                observed.value, output.value,
                "model rejection replaced the observed result"
            );
            assert_eq!(observed.images, output.images);
            assert_eq!(observed.is_error, output.is_error);
            let saved = crate::TranscriptProjection::from_session(&view);
            let activity = saved
                .items
                .iter()
                .find_map(|item| match item {
                    crate::TranscriptItem::ActivityGroup(group) => group.activities.first(),
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                activity.state,
                if output.is_error {
                    crate::ActivityState::Failed
                } else {
                    crate::ActivityState::Completed
                }
            );
            assert_eq!(activity.result.as_ref().unwrap().value, output.value);
            assert!(
                activity
                    .result
                    .as_ref()
                    .unwrap()
                    .projection
                    .and_then(crate::ToolResultProjection::notice)
                    .is_some()
            );
            assert_eq!(live.lock().unwrap().projection(), &saved);
            assert!(
                matches!(&view.display_messages()[2].content[0], Content::ToolResult(raw) if raw.result == output.value && raw.images == output.images)
            );
            drop(session);
            let reopened = Arc::new(CodingSession::open(&path).unwrap());
            assert_eq!(
                crate::TranscriptProjection::from_session(&reopened.view().unwrap()),
                saved
            );
            assert_eq!(
                reopened.context_messages_for(&model()).unwrap(),
                requests[1]
                    .messages
                    .iter()
                    .cloned()
                    .chain([Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("done".into())],
                        provider_replay: None
                    }])
                    .collect::<Vec<_>>()
            );
            let context = reopened.context_messages_for(&model()).unwrap();
            let other_model = ModelRef {
                model: "other".into(),
                ..model()
            };
            assert_eq!(
                reopened.context_messages_for(&other_model).unwrap(),
                context
            );
            for copy in [
                reopened.clone_to(root.join("clone.sqlite")).unwrap(),
                reopened
                    .fork_to(root.join("fork.sqlite"), ForkPoint::AfterTurn(1))
                    .unwrap(),
            ] {
                assert_eq!(copy.context_messages_for(&model()).unwrap(), context);
                assert_eq!(
                    crate::TranscriptProjection::from_session(&copy.view().unwrap()),
                    saved
                );
            }
            assert!(
                agent
                    .compact(reopened.clone(), CancellationToken::new(), |_| {})
                    .await
                    .unwrap()
            );
            let summary_request = &scripts.requests()[2];
            let summary_result = summary_request.messages[0]
                .content
                .iter()
                .find_map(|part| {
                    let Content::Text(text) = part else {
                        return None;
                    };
                    let record: serde_json::Value =
                        serde_json::from_str(text.split_once('\n').unwrap().1).unwrap();
                    (record["role"] == "Tool").then(|| record["content"][0]["ToolResult"].clone())
                })
                .unwrap();
            assert_eq!(summary_result["is_error"], output.is_error);
            assert_eq!(summary_result["result"], model_result.result);
            let summary_request = serde_json::to_string(summary_request).unwrap();
            if let Some(report) = output.value["report"].as_str() {
                assert!(!summary_request.contains(report));
            }
            for image in &output.images {
                assert!(!summary_request.contains(image.data()));
            }
            assert_eq!(
                crate::TranscriptProjection::from_session(&reopened.view().unwrap()),
                saved
            );
            drop(reopened);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn tool_images_are_committed_replayed_and_rejected_on_a_text_only_route() {
        struct ImageTool;
        impl ToolSource for ImageTool {
            fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
                let definitions = {
                    vec![ToolDefinition::external(ToolSpec {
                        name: "picture".into(),
                        description: "picture".into(),
                        input_schema: serde_json::json!({"type":"object"}),
                    })]
                };
                definitions
                    .into_iter()
                    .map(|definition| ToolRegistration::new(definition, self.clone()))
                    .collect()
            }
        }

        impl ToolExecutor for ImageTool {
            fn execute<'a>(
                &'a self,
                _invocation: crate::ToolInvocation<'a>,
            ) -> BoxFuture<'a, ToolOutput> {
                Box::pin(async {
                    ToolOutput {
                        value: serde_json::json!({"path":"picture.png"}),
                        images: vec![tiny_image()],
                        is_error: false,
                    }
                })
            }
        }
        let root = std::env::temp_dir().join(format!("ion-tool-image-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let call = || {
            response(vec![Content::ToolCall(ToolCall {
                id: "picture-call".into(),
                name: "picture".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
            })])
        };
        let scripts = Arc::new(ScriptedModelService::new([
            call(),
            response(vec![Content::Text("saw it".into())]),
        ]));
        let agent =
            Agent::new(scripts.clone(), Arc::new(ImageTool), model()).with_limits(AgentLimits {
                image_input: true,
                ..AgentLimits::default()
            });
        agent
            .submit(
                session.clone(),
                "inspect".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert!(
            matches!(&scripts.requests()[1].messages[2].content[0], Content::ToolResult(result) if result.images.len() == 1)
        );
        drop(session);
        let reopened = Arc::new(CodingSession::open(&path).unwrap());
        assert!(
            matches!(&reopened.messages().unwrap()[2].content[0], Content::ToolResult(result) if result.images.len() == 1)
        );
        let text_service = Arc::new(ScriptedModelService::new([]));
        let text_agent = Agent::new(text_service.clone(), Arc::new(ImageTool), model());
        let error = text_agent
            .submit(
                reopened.clone(),
                "continue".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::ImagesUnsupported));
        assert!(text_service.requests().is_empty());
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn long_coding_turn_is_not_stopped_by_an_arbitrary_step_count() {
        let root = std::env::temp_dir().join(format!("ion-long-turn-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file.txt"), "content").unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let mut scripts = (0..81)
            .map(|index| {
                response(vec![Content::ToolCall(ToolCall {
                    id: format!("read-{index}"),
                    name: "read".into(),
                    arguments: serde_json::json!({"path":"file.txt"}),
                    raw_arguments: None,
                })])
            })
            .collect::<Vec<_>>();
        scripts.push(response(vec![Content::Text("done".into())]));
        let service = Arc::new(ScriptedModelService::new(scripts));
        let agent = Agent::new(service.clone(), Arc::new(TestTools::new(&root)), model());
        let answer = agent
            .submit(
                session.clone(),
                "inspect the file repeatedly".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(answer, "done");
        assert_eq!(service.requests().len(), 82);
        assert!(session.view().unwrap().unfinished_turn.is_none());
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn fast_model_steps_leave_room_for_cancellation() {
        let root = std::env::temp_dir().join(format!("ion-fast-turn-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file.txt"), "content").unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let scripts = (0..100)
            .map(|index| {
                response(vec![Content::ToolCall(ToolCall {
                    id: format!("read-{index}"),
                    name: "read".into(),
                    arguments: serde_json::json!({"path":"file.txt"}),
                    raw_arguments: None,
                })])
            })
            .collect::<Vec<_>>();
        let service = Arc::new(ScriptedModelService::new(scripts));
        let agent = Agent::new(service.clone(), Arc::new(TestTools::new(&root)), model());
        let stop = CancellationToken::new();
        let mut trigger = Some(stop.clone());
        let result = agent
            .submit(
                session.clone(),
                "inspect the file".into(),
                "test".into(),
                stop,
                move |event| {
                    if matches!(event, AgentEvent::ToolFinished { .. })
                        && let Some(trigger) = trigger.take()
                    {
                        tokio::spawn(async move { trigger.cancel() });
                    }
                },
            )
            .await;
        assert!(matches!(result, Err(AgentError::Cancelled)));
        assert!(service.requests().len() < 100);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn empty_completed_responses_fail_without_poisoning_resume() {
        let root =
            std::env::temp_dir().join(format!("ion-empty-response-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let service = Arc::new(ScriptedModelService::new([
            response(Vec::new()),
            response(vec![Content::Text(" \n".into())]),
            response(vec![Content::Text("working again".into())]),
        ]));
        let agent = Agent::new(service, Arc::new(TestTools::new(&root)), model());
        for prompt in ["empty", "blank"] {
            let error = agent
                .submit(
                    session.clone(),
                    prompt.into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap_err();
            assert!(matches!(error, AgentError::IncompleteModelResponse));
        }
        assert!(
            !session
                .messages()
                .unwrap()
                .iter()
                .any(|message| message.role == Role::Assistant)
        );
        drop(session);
        let reopened = Arc::new(CodingSession::open(&path).unwrap());
        assert_eq!(
            agent
                .submit(
                    reopened.clone(),
                    "continue".into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap(),
            "working again"
        );
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn truncated_tool_call_is_rejected_and_reissued_without_effect() {
        let root =
            std::env::temp_dir().join(format!("ion-truncated-call-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![
                        Content::ToolCall(ToolCall {
                            id: "partial".into(),
                            name: "write".into(),
                            arguments: serde_json::json!({"path":"should-not-exist","content":"wrong"}),
                            raw_arguments: None,
                        }),
                        Content::ToolCall(ToolCall {
                            id: "partial-2".into(),
                            name: "exec".into(),
                            arguments: serde_json::json!({"command":"touch should-not-exist-2"}),
                            raw_arguments: None,
                        }),
                    ],
                    provider_replay: None,
                },
                usage: Usage::known(10, 20),
                termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
                returned_model: None,
            })]),
            response(vec![Content::ToolCall(ToolCall {
                id: "complete".into(),
                name: "write".into(),
                arguments: serde_json::json!({"path":"created.txt","content":"right"}),
                raw_arguments: None,
            })]),
            response(vec![Content::Text("done".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model());
        let rejected = Arc::new(Mutex::new(Vec::new()));
        let observed_rejected = rejected.clone();
        assert_eq!(
            agent
                .submit(
                    session.clone(),
                    "write the file".into(),
                    "test".into(),
                    CancellationToken::new(),
                    move |event| {
                        if let AgentEvent::ToolFinished {
                            call_id,
                            outcome: crate::ToolOutcome::NotDispatched { .. },
                            ..
                        } = event
                        {
                            observed_rejected.lock().unwrap().push(call_id);
                        }
                    },
                )
                .await
                .unwrap(),
            "done"
        );
        assert_eq!(*rejected.lock().unwrap(), ["partial", "partial-2"]);
        assert!(!root.join("should-not-exist").exists());
        assert!(!root.join("should-not-exist-2").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("created.txt")).unwrap(),
            "right"
        );
        assert!(matches!(
            &scripts.requests()[1].messages[1].content[0],
            Content::ToolCall(call) if call.id == "partial"
        ));
        assert!(matches!(
            &scripts.requests()[1].messages[2].content[0],
            Content::ToolResult(result) if result.is_error && result.call_id == "partial"
        ));
        assert!(matches!(
            &scripts.requests()[1].messages[3].content[0],
            Content::ToolResult(result) if result.is_error && result.call_id == "partial-2"
        ));
        assert!(session.view().unwrap().entries.iter().any(|entry| matches!(
            entry,
            crate::session::SessionEntry::Assistant {
                termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
                ..
            }
        )));
        drop(session);
        let reopened = Arc::new(CodingSession::open(&path).unwrap());
        assert!(reopened.view().unwrap().unfinished_turn.is_none());
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rejected_steering_stays_available_to_the_host() {
        let root =
            std::env::temp_dir().join(format!("ion-steering-fault-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([response(vec![Content::Text(
            "done".into(),
        )])]));
        let agent = Agent::new(scripts, Arc::new(TestTools::new(&root)), model()).with_limits(
            AgentLimits {
                max_request_bytes: 80 * 1024 * 1024,
                ..AgentLimits::default()
            },
        );
        let steering = Arc::new(SteeringInbox::new(
            agent.limits(),
            crate::InputBudget::new(128 * 1024 * 1024),
        ));
        let prompt = "\0".repeat(11 * 1024 * 1024);
        steering.push(prompt.clone()).unwrap();
        let result = agent
            .submit_with_steering(
                session.clone(),
                "start".into(),
                "test".into(),
                CancellationToken::new(),
                steering.clone(),
                |_| {},
            )
            .await;
        assert!(matches!(result, Err(AgentError::Session(_))));
        assert_eq!(
            steering
                .take_uncommitted()
                .into_iter()
                .map(crate::AcceptedInput::into_message)
                .collect::<Vec<_>>(),
            vec![user_text(prompt)]
        );
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn assistant_and_steering_commit_together_or_remain_unpublished() {
        let root =
            std::env::temp_dir().join(format!("ion-steering-batch-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([Script::Stream(vec![
            ModelStreamEvent::TextDelta("done".into()),
            ModelStreamEvent::Completed(ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("done".into())],
                    provider_replay: None,
                },
                usage: Usage::unknown(),
                termination: ResponseTermination::Completed,
                returned_model: Some("test".into()),
            }),
        ])]));
        let agent = Agent::new(scripts, Arc::new(TestTools::new(&root)), model()).with_limits(
            AgentLimits {
                max_request_bytes: 80 * 1024 * 1024,
                ..AgentLimits::default()
            },
        );
        let steering = Arc::new(SteeringInbox::new(
            agent.limits(),
            crate::InputBudget::new(128 * 1024 * 1024),
        ));
        let prompt = "\0".repeat(11 * 1024 * 1024);
        let published = Arc::new(Mutex::new(0));
        let observed_published = published.clone();
        let observed_steering = steering.clone();
        let observed_prompt = prompt.clone();
        let result = agent
            .submit_with_steering(
                session.clone(),
                "start".into(),
                "test".into(),
                CancellationToken::new(),
                steering.clone(),
                move |event| {
                    if matches!(
                        &event,
                        AgentEvent::AssistantCommitted { .. }
                            | AgentEvent::SteeringCommitted { .. }
                    ) {
                        *observed_published.lock().unwrap() += 1;
                    }
                    if matches!(event, AgentEvent::TextDelta(_)) {
                        observed_steering.push(observed_prompt.clone()).unwrap();
                    }
                },
            )
            .await;
        assert!(matches!(result, Err(AgentError::Session(_))));
        assert_eq!(
            steering
                .take_uncommitted()
                .into_iter()
                .map(crate::AcceptedInput::into_message)
                .collect::<Vec<_>>(),
            vec![user_text(prompt)]
        );
        assert_eq!(
            *published.lock().unwrap(),
            0,
            "failed commit cannot publish authoritative facts"
        );
        let entries = session.view().unwrap().entries;
        assert_eq!(
            entries
                .iter()
                .filter(|entry| matches!(
                    entry,
                    crate::session::SessionEntry::TurnStarted { .. }
                        | crate::session::SessionEntry::ModelContextChanged { .. }
                ))
                .count(),
            2
        );
        assert!(!entries.iter().any(|entry| matches!(
            entry,
            crate::session::SessionEntry::Assistant { .. }
                | crate::session::SessionEntry::Steering { .. }
        )));
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn final_text_and_queued_steering_continue_the_same_turn() {
        let root =
            std::env::temp_dir().join(format!("ion-steering-final-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let first = ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::Text("first".into())],
                provider_replay: None,
            },
            usage: Usage::unknown(),
            termination: ResponseTermination::Completed,
            returned_model: Some("test".into()),
        };
        let scripts = Arc::new(ScriptedModelService::new([
            Script::Stream(vec![
                ModelStreamEvent::TextDelta("first".into()),
                ModelStreamEvent::Completed(first),
            ]),
            response(vec![Content::Text("second".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model());
        let steering = Arc::new(SteeringInbox::new(
            agent.limits(),
            crate::InputBudget::default(),
        ));
        let observed_steering = steering.clone();
        let answer = agent
            .submit_with_steering(
                session.clone(),
                "start".into(),
                "test".into(),
                CancellationToken::new(),
                steering.clone(),
                move |event| {
                    if matches!(event, AgentEvent::TextDelta(_)) {
                        observed_steering.push("also answer this".into()).unwrap();
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(answer, "second");
        assert!(steering.take_uncommitted().is_empty());
        assert_eq!(scripts.requests().len(), 2);
        let entries = session.view().unwrap().entries;
        let kinds = entries
            .iter()
            .map(|entry| match entry {
                crate::session::SessionEntry::TurnStarted { .. } => "turn_started",
                crate::session::SessionEntry::ModelContextChanged { .. } => "model_context",
                crate::session::SessionEntry::Assistant { .. } => "assistant",
                crate::session::SessionEntry::Steering { .. } => "steering",
                crate::session::SessionEntry::TurnEnded { .. } => "turn_ended",
                _ => "other",
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "turn_started",
                "model_context",
                "assistant",
                "steering",
                "assistant",
                "turn_ended"
            ]
        );
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn committed_transcript_survives_steering_and_response_restart() {
        use crate::{LiveTranscript, SessionEntry, TranscriptItem, TranscriptProjection};

        let root =
            std::env::temp_dir().join(format!("ion-transcript-restart-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("earlier work".into())]),
            Script::Stream(vec![
                ModelStreamEvent::TextDelta("first".into()),
                ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("first".into())],
                        provider_replay: None,
                    },
                    usage: Usage::unknown(),
                    termination: ResponseTermination::Completed,
                    returned_model: None,
                }),
            ]),
            Script::Stream(vec![
                ModelStreamEvent::TextDelta("discard this attempt".into()),
                ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("discard this attempt".into())],
                        provider_replay: None,
                    },
                    usage: Usage::known(100, 10),
                    termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
                    returned_model: None,
                }),
            ]),
            response(vec![Content::Text("Earlier work summarized.".into())]),
            response(vec![Content::Text("second".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model());
        agent
            .submit(
                session.clone(),
                "earlier".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();

        let input = user_text("start".into());
        let live = Arc::new(Mutex::new(LiveTranscript::with_user_input(&input)));
        let steering = Arc::new(SteeringInbox::new(
            agent.limits(),
            crate::InputBudget::default(),
        ));
        steering
            .push("queued before the first step".into())
            .unwrap();
        let restarted = Arc::new(Mutex::new(false));
        let observed_restarted = restarted.clone();
        let observed_live = live.clone();
        let observed_steering = steering.clone();
        let observed_session = session.clone();
        let answer = agent
            .submit_with_steering(
                session.clone(),
                "start".into(),
                "test".into(),
                CancellationToken::new(),
                steering.clone(),
                move |event| {
                    if matches!(&event, AgentEvent::TextDelta(text) if text == "first") {
                        observed_steering
                            .push("after the first answer".into())
                            .unwrap();
                    }
                    let restart = matches!(&event, AgentEvent::ResponseRestarted);
                    let mut live = observed_live.lock().unwrap();
                    live.observe(&event);
                    if restart {
                        *observed_restarted.lock().unwrap() = true;
                        let view = observed_session.view().unwrap();
                        let turn = view.unfinished_turn.unwrap();
                        let committed = TranscriptProjection::from_session(&view);
                        let prefix = committed
                            .items
                            .into_iter()
                            .filter(|item| match item {
                                TranscriptItem::User(message)
                                | TranscriptItem::Assistant(message) => message.turn == Some(turn),
                                TranscriptItem::ActivityGroup(group) => group.turn == turn,
                                TranscriptItem::UserShell(_) => false,
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(
                            live.projection().items,
                            prefix,
                            "restart must retain committed assistant and steering"
                        );
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(answer, "second");
        assert!(*restarted.lock().unwrap());
        let live = live.lock().unwrap();
        assert!(steering.take_uncommitted().is_empty());
        let requests = scripts.requests();
        assert_eq!(requests.len(), 5);
        assert!(requests[3].tools.is_empty());
        let turn = session
            .view()
            .unwrap()
            .entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                SessionEntry::TurnStarted { turn, .. } => Some(*turn),
                _ => None,
            })
            .unwrap();
        drop(session);
        let reopened = Arc::new(CodingSession::open(&path).unwrap());
        let durable = TranscriptProjection::from_session(&reopened.view().unwrap());
        let items = durable
            .items
            .into_iter()
            .filter(|item| match item {
                TranscriptItem::User(message) | TranscriptItem::Assistant(message) => {
                    message.turn == Some(turn)
                }
                TranscriptItem::ActivityGroup(group) => group.turn == turn,
                TranscriptItem::UserShell(_) => false,
            })
            .collect::<Vec<_>>();
        assert_eq!(live.projection().items, items);
        let visible = live
            .projection()
            .items
            .iter()
            .map(|item| {
                let (role, message) = match item {
                    TranscriptItem::User(message) => ("user", message),
                    TranscriptItem::Assistant(message) => ("assistant", message),
                    _ => panic!("this Turn has only conversation messages"),
                };
                let [crate::TranscriptPart::Text(text)] = message.parts.as_slice() else {
                    panic!("expected one text part");
                };
                (role, message.steering, text.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            visible,
            [
                ("user", false, "start"),
                ("user", true, "queued before the first step"),
                ("assistant", false, "first"),
                ("user", true, "after the first answer"),
                ("assistant", false, "second"),
            ]
        );
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn image_steering_is_durable_and_replayed_in_the_next_model_step() {
        let root =
            std::env::temp_dir().join(format!("ion-image-steering-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let image = tiny_image();
        let first = ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::Text("first".into())],
                provider_replay: None,
            },
            usage: Usage::unknown(),
            termination: ResponseTermination::Completed,
            returned_model: Some("test".into()),
        };
        let scripts = Arc::new(ScriptedModelService::new([
            Script::Stream(vec![
                ModelStreamEvent::TextDelta("first".into()),
                ModelStreamEvent::Completed(first),
            ]),
            response(vec![Content::Text("second".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model())
            .with_limits(AgentLimits {
                image_input: true,
                ..AgentLimits::default()
            });
        let steering = Arc::new(SteeringInbox::new(
            agent.limits(),
            crate::InputBudget::default(),
        ));
        let observed_steering = steering.clone();
        let answer = agent
            .submit_with_steering(
                session.clone(),
                "start".into(),
                "test".into(),
                CancellationToken::new(),
                steering.clone(),
                move |event| {
                    if matches!(event, AgentEvent::TextDelta(_)) {
                        observed_steering
                            .push_message(
                                Message {
                                    role: Role::User,
                                    content: vec![
                                        Content::Text("look at this".into()),
                                        Content::Image(image.clone()),
                                    ],
                                    provider_replay: None,
                                },
                                (),
                            )
                            .unwrap();
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(answer, "second");
        assert!(steering.take_uncommitted().is_empty());
        let requests = scripts.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|part| matches!(part, Content::Image(_)))
        }));
        let view = session.view().unwrap();
        assert!(view.entries.iter().any(|entry| matches!(entry, crate::session::SessionEntry::Steering { input, .. } if input.content.iter().any(|part| matches!(part, Content::Image(_))))));
        drop(session);
        let reopened = Arc::new(CodingSession::open(&path).unwrap());
        assert!(reopened.context_messages().unwrap().iter().any(|message| {
            message
                .content
                .iter()
                .any(|part| matches!(part, Content::Image(_)))
        }));
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn steering_commit_releases_host_metadata_outside_inbox_lock() {
        use std::sync::{
            Weak,
            atomic::{AtomicUsize, Ordering},
        };
        #[derive(serde::Serialize)]
        struct Metadata {
            literal: String,
            #[serde(skip)]
            inbox: Weak<SteeringInbox<Metadata>>,
            #[serde(skip)]
            dropped: Arc<AtomicUsize>,
        }
        impl Drop for Metadata {
            fn drop(&mut self) {
                let inbox = self.inbox.upgrade().unwrap();
                assert!(
                    inbox.pending.try_lock().is_ok(),
                    "metadata dropped under inbox lock"
                );
                self.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }
        let root =
            std::env::temp_dir().join(format!("ion-steering-metadata-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        for assistant in [false, true] {
            let session =
                CodingSession::create(root.join(format!("{assistant}.sqlite")), Some(&root))
                    .unwrap();
            let (turn, _) = session
                .begin_turn_message(user_text("start".into()), model())
                .unwrap();
            let dropped = Arc::new(AtomicUsize::new(0));
            let inbox = Arc::new(SteeringInbox::new(
                AgentLimits::default(),
                crate::InputBudget::default(),
            ));
            inbox
                .push_message(
                    user_text("prepared".into()),
                    Metadata {
                        literal: "PRIVATE_LITERAL".into(),
                        inbox: Arc::downgrade(&inbox),
                        dropped: dropped.clone(),
                    },
                )
                .unwrap();
            let messages = if assistant {
                inbox
                    .record_assistant(
                        &session,
                        turn,
                        Message {
                            role: Role::Assistant,
                            content: vec![Content::Text("answer".into())],
                            provider_replay: None,
                        },
                        vec![],
                        ModelExecution {
                            route: ModelRoute::direct(model(), ModelRouteReason::UserRequest),
                            returned_model: Some("test".into()),
                        },
                        ion_ai::Usage::unknown(),
                        AgentLimits::default(),
                    )
                    .unwrap()
                    .1
            } else {
                inbox
                    .record_pending(&session, turn, AgentLimits::default())
                    .unwrap()
            };
            assert_eq!(messages, vec![user_text("prepared".into())]);
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
            assert!(inbox.take_uncommitted().is_empty());
            assert!(
                !serde_json::to_string(&session.view().unwrap().messages)
                    .unwrap()
                    .contains("PRIVATE_LITERAL")
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn image_steering_cannot_commit_on_a_text_only_route() {
        let root =
            std::env::temp_dir().join(format!("ion-image-steering-route-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap();
        let (turn, _) = session
            .begin_turn_message(user_text("start".into()), model())
            .unwrap();
        let steering = SteeringInbox::new(
            AgentLimits {
                image_input: true,
                ..AgentLimits::default()
            },
            crate::InputBudget::default(),
        );
        let input = Message {
            role: Role::User,
            content: vec![Content::Text("look".into()), Content::Image(tiny_image())],
            provider_replay: None,
        };
        steering.push_message(input.clone(), ()).unwrap();
        assert!(matches!(
            steering.record_pending(&session, turn, AgentLimits::default()),
            Err(AgentError::ImagesUnsupported)
        ));
        assert_eq!(
            steering
                .take_uncommitted()
                .into_iter()
                .map(crate::AcceptedInput::into_message)
                .collect::<Vec<_>>(),
            vec![input]
        );
        assert_eq!(session.view().unwrap().entries.len(), 1);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn steering_is_recorded_before_the_next_model_step_in_one_turn() {
        use crate::{LiveTranscript, TranscriptProjection};

        let root = std::env::temp_dir().join(format!("ion-steering-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![
                Content::Text("Before the call".into()),
                Content::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path":"file.txt"}),
                    raw_arguments: None,
                }),
                Content::Text("After the call".into()),
            ]),
            response(vec![Content::Text("done".into())]),
        ]));
        std::fs::write(root.join("file.txt"), "content").unwrap();
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model());
        let steering = Arc::new(SteeringInbox::new(
            agent.limits(),
            crate::InputBudget::default(),
        ));
        let live = Arc::new(Mutex::new(LiveTranscript::with_user_input(&user_text(
            "read file".into(),
        ))));
        let observed_live = live.clone();
        let observed_steering = steering.clone();
        agent
            .submit_with_steering(
                session.clone(),
                "read file".into(),
                "test".into(),
                CancellationToken::new(),
                steering.clone(),
                move |event| {
                    if matches!(&event, AgentEvent::ToolFinished { .. }) {
                        observed_steering
                            .push("also check the content".into())
                            .unwrap();
                    }
                    observed_live.lock().unwrap().observe(&event);
                },
            )
            .await
            .unwrap();
        let requests = scripts.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].messages.last().unwrap().role, Role::User);
        assert_eq!(
            session
                .view()
                .unwrap()
                .entries
                .iter()
                .filter(|entry| matches!(entry, crate::session::SessionEntry::Steering { .. }))
                .count(),
            1
        );
        assert_eq!(
            session
                .view()
                .unwrap()
                .entries
                .iter()
                .filter(|entry| matches!(entry, crate::session::SessionEntry::TurnStarted { .. }))
                .count(),
            1
        );
        assert_eq!(
            *live.lock().unwrap().projection(),
            TranscriptProjection::from_session(&session.view().unwrap())
        );
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    struct ReasoningScripts(Arc<ScriptedModelService>);
    impl ModelService for ReasoningScripts {
        fn validate_controls(
            &self,
            _: &ModelRef,
            controls: &GenerationControls,
        ) -> Result<(), ProviderError> {
            controls.validate()?;
            if controls.reasoning == Reasoning::Medium && controls.tool_choice == ToolChoice::None {
                return Err(ProviderError {
                    kind: ProviderErrorKind::Unsupported,
                    message: "fixture summary cannot use medium effort".into(),
                    retry_after_ms: None,
                });
            }
            Ok(())
        }
        fn stream<'a>(
            &'a self,
            request: ModelRequest,
        ) -> BoxFuture<'a, Result<ion_ai::ModelStream, ProviderError>> {
            self.0.stream(request)
        }
    }

    #[tokio::test]
    async fn idle_preferences_survive_unfinished_shell_without_recovering_it() {
        let root =
            std::env::temp_dir().join(format!("ion-idle-preferences-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([response(vec![Content::Text(
            "continued".into(),
        )])]));
        let tools = Arc::new(TestTools::new(&root));
        let service = Arc::new(ReasoningScripts(scripts.clone()));
        let first = Agent::new(service.clone(), tools.clone(), model());
        first.select_model(&session).unwrap();
        let (admitted, admission) = tokio::sync::oneshot::channel();
        let (fail, failure) = tokio::sync::oneshot::channel();
        let operation = session.run_user_shell(
            "touch should-not-execute".into(),
            false,
            CancellationToken::new(),
            move |_, _| async move {
                admitted.send(()).unwrap();
                failure.await.unwrap();
                // Admission is durable, but no host effect has been attempted.
                // A genuine task failure leaves historical unfinished intent.
                panic!("fixture shell failed before any host effect")
            },
        );
        admission.await.unwrap();
        let replacement_model = ModelRef {
            model: "replacement".into(),
            ..model()
        };
        let replacement = Agent::new(service, tools, replacement_model.clone());
        assert!(matches!(
            replacement.select_model(&session),
            Err(AgentError::Session(SessionError::OperationActive))
        ));
        assert!(matches!(
            replacement.select_reasoning(&session, Reasoning::High),
            Err(AgentError::Session(SessionError::OperationActive))
        ));
        fail.send(()).unwrap();
        assert!(matches!(
            operation.await,
            Err(SessionError::Operation(crate::OperationError::TaskFailed(
                _
            )))
        ));
        drop(session);

        let session = Arc::new(CodingSession::open(&path).unwrap());
        let before = session.view().unwrap();
        assert!(before.unfinished_user_shell.is_some());
        replacement.select_model(&session).unwrap();
        let before_invalid = session.view().unwrap().entries;
        assert!(
            replacement
                .select_reasoning(&session, Reasoning::Medium)
                .is_err()
        );
        assert_eq!(session.view().unwrap().entries, before_invalid);
        replacement
            .select_reasoning(&session, Reasoning::High)
            .unwrap();
        let selected = session.view().unwrap();
        assert_eq!(selected.last_model, Some(replacement_model.clone()));
        assert_eq!(selected.reasoning, Reasoning::High);
        assert_eq!(selected.unfinished_user_shell, before.unfinished_user_shell);
        assert!(
            !selected.entries.iter().any(|entry| matches!(
                entry,
                crate::session::SessionEntry::UserShellSettled { .. }
            ))
        );
        assert!(scripts.requests().is_empty());
        drop(session);

        let session = Arc::new(CodingSession::open(&path).unwrap());
        assert_eq!(
            session.view().unwrap().last_model,
            Some(replacement_model.clone())
        );
        assert_eq!(session.reasoning().unwrap(), Reasoning::High);
        assert_eq!(
            session.view().unwrap().unfinished_user_shell,
            before.unfinished_user_shell
        );
        replacement
            .submit(
                session.clone(),
                "continue".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let after = session.view().unwrap();
        assert!(after.unfinished_user_shell.is_none());
        assert!(matches!(
            after.entries[selected.entries.len()],
            crate::session::SessionEntry::UserShellSettled {
                outcome: crate::session::UserShellOutcome::Unknown,
                ..
            }
        ));
        assert_eq!(after.turns()[0].model, replacement_model);
        assert_eq!(after.turns()[0].reasoning, Reasoning::High);
        assert_eq!(scripts.requests().len(), 1);
        assert!(!root.join("should-not-execute").exists());
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn manual_compaction_uses_no_tools_and_keeps_raw_history() {
        let root = std::env::temp_dir().join(format!("ion-compact-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("first result".into())]),
            response(vec![Content::Text("First task is complete.".into())]),
            response(vec![Content::Text("continued".into())]),
        ]));
        let agent = Agent::new(
            Arc::new(ReasoningScripts(scripts.clone())),
            Arc::new(TestTools::new(&root)),
            model(),
        );
        agent.select_reasoning(&session, Reasoning::High).unwrap();
        let before = session.view().unwrap().entries;
        assert!(agent.select_reasoning(&session, Reasoning::Medium).is_err());
        assert_eq!(session.view().unwrap().entries, before);
        assert!(scripts.requests().is_empty());
        agent
            .submit(
                session.clone(),
                "first task".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert!(
            agent
                .compact(session.clone(), CancellationToken::new(), |_| {})
                .await
                .unwrap()
        );
        assert!(scripts.requests()[1].tools.is_empty());
        let provider_id = session.provider_session_id().to_string();
        drop(session);
        let session = Arc::new(CodingSession::open(&path).unwrap());
        assert_eq!(session.provider_session_id().to_string(), provider_id);
        assert_eq!(session.messages().unwrap().len(), 2);
        assert_eq!(session.context_messages().unwrap().len(), 1);
        agent
            .submit(
                session.clone(),
                "continue".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let requests = scripts.requests();
        assert!(requests.iter().all(|request| {
            request.provider_session_id.as_deref() == Some(provider_id.as_str())
        }));
        assert_eq!(requests[2].messages.len(), 2);
        assert!(
            requests
                .iter()
                .all(|request| request.controls.reasoning == Reasoning::High)
        );
        assert_eq!(session.reasoning().unwrap(), Reasoning::High);
        assert_eq!(session.messages().unwrap().len(), 4);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn provider_overflow_compacts_and_retries_only_the_model_request() {
        let root = std::env::temp_dir().join(format!("ion-overflow-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("first result".into())]),
            Script::OpenError(ProviderError {
                kind: ProviderErrorKind::ContextLength,
                message: "too long".into(),
                retry_after_ms: None,
            }),
            response(vec![Content::Text("First result is complete.".into())]),
            response(vec![Content::Text("continued".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model());
        for prompt in ["first task", "next task"] {
            agent
                .submit(
                    session.clone(),
                    prompt.into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap();
        }
        let requests = scripts.requests();
        assert_eq!(requests.len(), 4);
        assert!(requests[2].tools.is_empty());
        assert_eq!(requests[3].messages.len(), 2);
        assert_eq!(session.messages().unwrap().len(), 4);
        assert!(session.view().unwrap().compacted_through.is_some());
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn completed_context_capacity_outcome_compacts_before_retry() {
        let root =
            std::env::temp_dir().join(format!("ion-completed-overflow-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("earlier result".into())]),
            Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: Vec::new(),
                    provider_replay: None,
                },
                usage: Usage::known(49_900, 0),
                termination: ResponseTermination::Incomplete(IncompleteReason::ContextLength),
                returned_model: None,
            })]),
            response(vec![Content::Text("Earlier result is complete.".into())]),
            response(vec![Content::Text("continued".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model())
            .with_limits(AgentLimits {
                context_window_tokens: Some(50_000),
                ..AgentLimits::default()
            });
        for prompt in ["first", "second"] {
            agent
                .submit(
                    session.clone(),
                    prompt.into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap();
        }
        assert_eq!(scripts.requests().len(), 4);
        assert!(scripts.requests()[2].tools.is_empty());
        assert!(session.view().unwrap().compacted_through.is_some());
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn context_recovery_does_not_reuse_observed_output() {
        let call = ToolCall {
            id: "partial".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
            raw_arguments: None,
        };
        for (output, content, replay) in [
            (vec![ModelStreamEvent::OutputObserved], vec![], None),
            (
                vec![ModelStreamEvent::ThinkingDelta {
                    block: 0,
                    text: "discarded thought".into(),
                }],
                vec![],
                None,
            ),
            (vec![ModelStreamEvent::ToolCall(call.clone())], vec![], None),
            (vec![], vec![Content::ToolCall(call)], None),
            (
                vec![],
                vec![],
                Some(ion_ai::ProviderReplay::new(
                    "test",
                    "opaque",
                    serde_json::json!("PRIVATE_OPAQUE"),
                )),
            ),
        ] {
            let human = output
                .iter()
                .any(|event| matches!(event, ModelStreamEvent::ThinkingDelta { .. }));
            let root =
                std::env::temp_dir().join(format!("ion-output-overflow-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&root).unwrap();
            let session =
                Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
            let scripts = Arc::new(ScriptedModelService::new([
                response(vec![Content::Text("earlier result".into())]),
                Script::Stream(
                    output
                        .into_iter()
                        .chain([ModelStreamEvent::Completed(ModelResponse {
                            message: Message {
                                role: Role::Assistant,
                                content,
                                provider_replay: replay,
                            },
                            usage: Usage::known(49_900, 0),
                            termination: ResponseTermination::Incomplete(
                                IncompleteReason::ContextLength,
                            ),
                            returned_model: None,
                        })])
                        .collect(),
                ),
                response(vec![Content::Text("must not summarize".into())]),
                response(vec![Content::Text("must not retry".into())]),
            ]));
            let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model());
            agent
                .submit(
                    session.clone(),
                    "first".into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap();
            let events = Arc::new(Mutex::new(Vec::new()));
            let observed_events = events.clone();
            let error = agent
                .submit(
                    session.clone(),
                    "second".into(),
                    "test".into(),
                    CancellationToken::new(),
                    move |event| observed_events.lock().unwrap().push(event),
                )
                .await
                .unwrap_err();
            assert!(matches!(error, AgentError::IncompleteModelResponse));
            let events = events.lock().unwrap();
            if human {
                assert!(events.iter().any(|event| matches!(event, AgentEvent::ThinkingDelta { text, .. } if text == "discarded thought")));
            } else {
                assert!(
                    events
                        .iter()
                        .any(|event| matches!(event, AgentEvent::ModelOutputObserved))
                );
            }
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, AgentEvent::ToolStarted { .. }))
            );
            assert_eq!(scripts.requests().len(), 2);
            assert!(session.view().unwrap().compacted_through.is_none());
            drop(session);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn short_length_stop_compacts_and_retries_without_dispatching_partial_call() {
        let root = std::env::temp_dir().join(format!("ion-short-length-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("earlier work".into())]),
            Script::Stream(vec![
                ModelStreamEvent::TextDelta("partial answer".into()),
                ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Content::ToolCall(ToolCall {
                            id: "partial".into(),
                            name: "write".into(),
                            arguments: serde_json::json!({"path":"never-created","content":"wrong"}),
                            raw_arguments: None,
                        })],
                        provider_replay: None,
                    },
                    usage: Usage::known(100, 10),
                    termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
                    returned_model: None,
                }),
            ]),
            response(vec![Content::Text("Earlier work summarized.".into())]),
            response(vec![Content::Text("complete answer".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model());
        agent
            .submit(
                session.clone(),
                "first".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let restarted = Arc::new(Mutex::new(false));
        let observed_restarted = restarted.clone();
        let answer = agent
            .submit(
                session.clone(),
                "second".into(),
                "test".into(),
                CancellationToken::new(),
                move |event| {
                    if matches!(event, AgentEvent::ResponseRestarted) {
                        *observed_restarted.lock().unwrap() = true;
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(answer, "complete answer");
        assert!(*restarted.lock().unwrap());
        assert!(!root.join("never-created").exists());
        let requests = scripts.requests();
        assert_eq!(requests.len(), 4);
        assert!(requests[2].tools.is_empty());
        assert!(session.view().unwrap().compacted_through.is_some());
        assert!(
            !session.view().unwrap().entries.iter().any(|entry| matches!(
                entry,
                crate::session::SessionEntry::Assistant {
                    termination: ResponseTermination::Incomplete(_),
                    ..
                }
            ))
        );
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn full_dispatched_output_budget_does_not_trigger_context_recovery() {
        struct FillBudget {
            requests: Mutex<Vec<ModelRequest>>,
        }

        impl ModelService for FillBudget {
            fn stream<'a>(
                &'a self,
                request: ModelRequest,
            ) -> BoxFuture<'a, Result<ion_ai::ModelStream, ProviderError>> {
                Box::pin(async move {
                    let budget = request.controls.max_output_tokens;
                    let step = {
                        let mut requests = self.requests.lock().unwrap();
                        let step = requests.len();
                        requests.push(request);
                        step
                    };
                    let (text, termination, output) = match step {
                        0 => ("earlier work", ResponseTermination::Completed, 1),
                        1 => (
                            "unfinished",
                            ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
                            u64::from(budget),
                        ),
                        2 => ("unexpected summary", ResponseTermination::Completed, 1),
                        _ => ("unexpected retry", ResponseTermination::Completed, 1),
                    };
                    let event = ModelStreamEvent::Completed(ModelResponse {
                        message: Message {
                            role: Role::Assistant,
                            content: vec![Content::Text(text.into())],
                            provider_replay: None,
                        },
                        usage: Usage::known(100, output),
                        termination,
                        returned_model: None,
                    });
                    let stream: ion_ai::ModelStream =
                        Box::pin(futures_util::stream::iter([Ok(event)]));
                    Ok(stream)
                })
            }
        }

        for (ceiling, window) in [(128, None), (16_384, Some(20_000))] {
            let root =
                std::env::temp_dir().join(format!("ion-full-length-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&root).unwrap();
            let session =
                Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
            let model_service = Arc::new(FillBudget {
                requests: Mutex::new(Vec::new()),
            });
            let agent = Agent::new(
                model_service.clone(),
                Arc::new(TestTools::new(&root)),
                model(),
            )
            .with_limits(AgentLimits {
                max_output_tokens: ceiling,
                context_window_tokens: window,
                ..AgentLimits::default()
            });
            agent
                .submit(
                    session.clone(),
                    "first".into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap();
            let result = agent
                .submit(
                    session.clone(),
                    "second".into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await;
            assert!(
                matches!(result, Err(AgentError::IncompleteModelResponse)),
                "{result:?}"
            );
            let requests = model_service.requests.lock().unwrap();
            assert_eq!(
                requests.len(),
                2,
                "ordinary output exhaustion must not issue a summary or retry"
            );
            let dispatched = requests[1].controls.max_output_tokens;
            if window.is_some() {
                assert!(
                    dispatched < ceiling,
                    "this case must exercise a context-clamped budget"
                );
            } else {
                assert_eq!(dispatched, ceiling);
            }
            assert!(session.view().unwrap().compacted_through.is_none());
            drop(requests);
            drop(session);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn invented_tool_returns_error_and_model_can_correct_it() {
        let root = std::env::temp_dir().join(format!("ion-invented-tool-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::ToolCall(ToolCall {
                id: "invented".into(),
                name: "not_a_tool".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
            })]),
            response(vec![Content::Text("I can use read instead.".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model());
        let answer = agent
            .submit(
                session.clone(),
                "inspect".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(answer, "I can use read instead.");
        assert_eq!(scripts.requests().len(), 2);
        let result = &scripts.requests()[1].messages[2].content[0];
        assert!(
            matches!(result, Content::ToolResult(result) if result.is_error && result.result["error"].as_str().is_some_and(|error| error.contains("tool was not declared for this request: not_a_tool")))
        );
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cancellation_durably_rejects_only_unstarted_calls() {
        struct ConstructorEffect(std::path::PathBuf);
        impl ToolSource for ConstructorEffect {
            fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
                vec![ToolRegistration::new(
                    ToolDefinition::external(ToolSpec {
                        name: "effect".into(),
                        description: "record constructor authority".into(),
                        input_schema: serde_json::json!({"type":"object"}),
                    }),
                    self,
                )]
            }
        }
        impl ToolExecutor for ConstructorEffect {
            fn execute<'a>(
                &'a self,
                invocation: crate::ToolInvocation<'a>,
            ) -> BoxFuture<'a, ToolOutput> {
                let call = invocation.call();
                // Deliberately synchronous: selecting a route must not run this.
                std::fs::write(self.0.join(&call.id), "observed effect").unwrap();
                Box::pin(async {
                    ToolOutput {
                        // The words are not execution truth; this is an observed error.
                        value: serde_json::json!({"error":"The tool result was not committed. Its external effect is unknown; inspect the working directory before retrying."}),
                        images: Vec::new(),
                        is_error: true,
                    }
                })
            }
        }
        for (at_admission, closure_fault) in [(true, false), (false, false), (true, true)] {
            let root = std::env::temp_dir().join(format!("ion-unstarted-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&root).unwrap();
            let path = root.join("session.sqlite");
            let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
            if closure_fault {
                rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TRIGGER reject_close BEFORE INSERT ON entries WHEN json_extract(NEW.body,'$.kind')='turn_ended' BEGIN SELECT RAISE(ABORT,'closure unavailable'); END;").unwrap();
            }
            let tools = Arc::new(ConstructorEffect(root.clone()));
            let scripts = Arc::new(ScriptedModelService::new([
                response(
                    ["first", "unused"]
                        .into_iter()
                        .map(|id| {
                            Content::ToolCall(ToolCall {
                                id: id.into(),
                                name: "effect".into(),
                                arguments: serde_json::json!({}),
                                raw_arguments: None,
                            })
                        })
                        .collect(),
                ),
                response(vec![Content::Text("recovered without dispatch".into())]),
            ]));
            let agent = Agent::new(scripts.clone(), tools.clone(), model());
            let stop = CancellationToken::new();
            let live = crate::LiveTranscript::with_user_input(&Message::user_input(
                "cancel".into(),
                std::iter::empty(),
            ));
            let observations = Arc::new(Mutex::new((live, 0, 0)));
            let observed = observations.clone();
            let result = agent
                .submit(
                    session.clone(),
                    "cancel".into(),
                    "test".into(),
                    stop.clone(),
                    move |event| {
                        let mut observed = observed.lock().unwrap();
                        let (live, started, closures) = &mut *observed;
                        if (at_admission && matches!(event, AgentEvent::AssistantCommitted { .. }))
                            || (!at_admission
                                && matches!(
                                    event,
                                    AgentEvent::ToolFinished {
                                        outcome: crate::ToolOutcome::Observed { .. },
                                        ..
                                    }
                                ))
                        {
                            stop.cancel();
                        }
                        if matches!(event, AgentEvent::ToolStarted { .. }) {
                            *started += 1;
                        }
                        if matches!(
                            event,
                            AgentEvent::ToolFinished { .. } | AgentEvent::TurnEnded { .. }
                        ) {
                            *closures += 1;
                        }
                        live.observe(&event);
                    },
                )
                .await;
            let (started, closures) = {
                let observed = observations.lock().unwrap();
                (observed.1, observed.2)
            };
            if closure_fault {
                assert!(matches!(result, Err(AgentError::Session(_))), "{result:?}");
                assert_eq!(closures, 0, "failed closure published uncommitted facts");
                let view = session.view().unwrap();
                assert_eq!(view.unfinished_turn, Some(1));
                assert!(!view.entries.iter().any(|entry| matches!(
                    entry,
                    crate::SessionEntry::ToolResult { .. } | crate::SessionEntry::TurnEnded { .. }
                )));
                assert!(!root.join("first").exists() && !root.join("unused").exists());
                rusqlite::Connection::open(&path)
                    .unwrap()
                    .execute_batch("DROP TRIGGER reject_close;")
                    .unwrap();
                agent
                    .submit(
                        session.clone(),
                        "explicit recovery".into(),
                        "test".into(),
                        CancellationToken::new(),
                        |_| {},
                    )
                    .await
                    .unwrap();
                let outcomes = session
                    .view()
                    .unwrap()
                    .entries
                    .into_iter()
                    .filter_map(|entry| match entry {
                        crate::SessionEntry::ToolResult { result, .. } => Some(result.outcome),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    outcomes,
                    vec![crate::ToolOutcome::Unknown, crate::ToolOutcome::Unknown],
                    "uncommitted knowledge cannot survive as a fact"
                );
                assert!(!root.join("first").exists() && !root.join("unused").exists());
                assert_eq!(scripts.requests().len(), 2);
                drop(session);
                std::fs::remove_dir_all(root).unwrap();
                continue;
            }
            assert!(matches!(result, Err(AgentError::Cancelled)), "{result:?}");
            assert_eq!(started, usize::from(!at_admission));
            assert_eq!(root.join("first").exists(), !at_admission);
            assert!(!root.join("unused").exists());
            assert_eq!(scripts.requests().len(), 1);
            let view = session.view().unwrap();
            let saved = crate::TranscriptProjection::from_session(&view);
            assert_eq!(observations.lock().unwrap().0.projection(), &saved);
            let states = saved
                .items
                .iter()
                .filter_map(|item| match item {
                    crate::TranscriptItem::ActivityGroup(group) => Some(
                        group
                            .activities
                            .iter()
                            .map(|activity| activity.state)
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>();
            assert_eq!(
                states,
                vec![
                    if at_admission {
                        crate::ActivityState::Rejected
                    } else {
                        crate::ActivityState::Failed
                    },
                    crate::ActivityState::Rejected
                ]
            );
            assert!(view.entries.iter().any(|entry| matches!(entry, crate::SessionEntry::ToolResult { result, .. }
                if result.call_id == "unused" && matches!(result.outcome, crate::ToolOutcome::NotDispatched { .. }))));
            for copy in [
                session.clone_to(root.join("clone.sqlite")).unwrap(),
                session
                    .fork_to(root.join("fork.sqlite"), ForkPoint::AfterTurn(1))
                    .unwrap(),
            ] {
                assert_eq!(
                    crate::TranscriptProjection::from_session(&copy.view().unwrap()),
                    saved
                );
            }
            drop(session);
            let reopened = Arc::new(CodingSession::open(&path).unwrap());
            assert_eq!(
                crate::TranscriptProjection::from_session(&reopened.view().unwrap()),
                saved
            );
            drop(reopened);
            // Even the public low-level route only grants authority when polled.
            let catalog = ToolSet::new([tools as Arc<dyn ToolSource>]).snapshot();
            let call = ToolCall {
                id: "lazy".into(),
                name: "effect".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
            };
            let future = catalog.execute_untracked(&call, CancellationToken::new());
            assert!(!root.join("lazy").exists());
            future.await;
            assert!(root.join("lazy").exists());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn malformed_tool_arguments_return_error_without_dispatch() {
        struct EffectTools(std::path::PathBuf);
        impl ToolSource for EffectTools {
            fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
                vec![ToolRegistration::new(
                    ToolDefinition::external(ToolSpec {
                        name: "exec".into(),
                        description: "Record a dispatched effect".into(),
                        input_schema: serde_json::json!({"type":"object"}),
                    }),
                    self,
                )]
            }
        }
        impl ToolExecutor for EffectTools {
            fn execute<'a>(
                &'a self,
                _invocation: crate::ToolInvocation<'a>,
            ) -> BoxFuture<'a, ToolOutput> {
                Box::pin(async move {
                    std::fs::write(self.0.join("effect"), "dispatched").unwrap();
                    ToolOutput {
                        value: serde_json::json!({"written":true}),
                        images: Vec::new(),
                        is_error: false,
                    }
                })
            }
        }
        let root =
            std::env::temp_dir().join(format!("ion-malformed-call-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let calls = [
            ToolCall {
                id: "raw".into(),
                name: "exec".into(),
                arguments: serde_json::json!({}),
                raw_arguments: Some("{\"command\":".into()),
            },
            ToolCall {
                id: "array".into(),
                name: "exec".into(),
                arguments: serde_json::json!([]),
                raw_arguments: None,
            },
        ];
        let scripts = Arc::new(ScriptedModelService::new([
            response(calls.iter().cloned().map(Content::ToolCall).collect()),
            response(vec![Content::Text("I need to correct the calls".into())]),
        ]));
        let tools = Arc::new(EffectTools(root.clone()));
        let catalog = ToolSet::new([tools.clone() as Arc<dyn ToolSource>]).snapshot();
        let agent = Agent::new(scripts, tools, model());
        agent
            .submit(
                session.clone(),
                "try a command".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert!(!root.join("effect").exists());
        let results = session
            .messages()
            .unwrap()
            .into_iter()
            .flat_map(|message| message.content)
            .filter_map(|part| match part {
                Content::ToolResult(result) => Some(result),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(results.len(), calls.len());
        assert!(results.iter().all(|result| {
            result.is_error
                && result.result["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("valid JSON object"))
        }));
        for call in &calls {
            assert!(
                catalog
                    .execute_untracked(call, CancellationToken::new())
                    .await
                    .is_error
            );
            assert!(!root.join("effect").exists());
        }
        let valid = ToolCall {
            id: "valid".into(),
            name: "exec".into(),
            arguments: serde_json::json!({}),
            raw_arguments: None,
        };
        assert!(
            !catalog
                .execute_untracked(&valid, CancellationToken::new())
                .await
                .is_error
        );
        assert_eq!(
            std::fs::read_to_string(root.join("effect")).unwrap(),
            "dispatched"
        );
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn compaction_projects_images_and_excludes_opaque_replay_from_its_budget() {
        for (vision, opaque) in [(true, false), (true, true), (false, true)] {
            let root = std::env::temp_dir()
                .join(format!("ion-summary-input-probe-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&root).unwrap();
            let session =
                Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
            let image = tiny_image();
            let (turn, _) = session
                .begin_turn_message(
                    Message {
                        role: Role::User,
                        content: vec![
                            Content::Text("describe this image".into()),
                            Content::Image(image.clone()),
                        ],
                        provider_replay: None,
                    },
                    model(),
                )
                .unwrap();
            session
                .record_assistant(
                    turn,
                    Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("image findings recorded".into())],
                        provider_replay: opaque.then(|| {
                            ion_ai::ProviderReplay::new(
                                model().provider,
                                "fixture",
                                serde_json::Value::String("x".repeat(32_768)),
                            )
                        }),
                    },
                    Usage::unknown(),
                    false,
                )
                .unwrap();
            let before = session.view().unwrap().entries;
            let scripts = Arc::new(ScriptedModelService::new([response(vec![Content::Text(
                "summary".into(),
            )])]));
            let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model())
                .with_limits(AgentLimits {
                    max_request_bytes: 4_096,
                    max_output_tokens: 128,
                    image_input: vision,
                    ..AgentLimits::default()
                });
            let outcome = agent
                .compact(session.clone(), CancellationToken::new(), |_| {})
                .await;
            let requests = scripts.requests();
            let typed_images = requests
                .iter()
                .flat_map(|r| &r.messages)
                .flat_map(|m| &m.content)
                .filter(|c| matches!(c, Content::Image(_)))
                .count();
            let image_as_text = requests
                .iter()
                .flat_map(|r| &r.messages)
                .flat_map(|m| &m.content)
                .any(|c| matches!(c, Content::Text(text) if text.contains(image.data())));
            assert!(outcome.unwrap());
            assert_eq!(requests.len(), 1);
            assert_eq!(typed_images, usize::from(vision));
            assert!(
                !image_as_text,
                "image bytes must not become plaintext transcript data"
            );
            assert!(
                requests[0]
                    .messages
                    .iter()
                    .all(|m| m.provider_replay.is_none())
            );
            let after = session.view().unwrap();
            assert_eq!(&after.entries[..before.len()], before.as_slice());
            assert!(after.compacted_through.is_some());
            let context_before_reopen = session.context_messages().unwrap();
            let path = session.path().to_path_buf();
            drop(session);
            let reopened = Arc::new(CodingSession::open(&path).unwrap());
            assert_eq!(reopened.context_messages().unwrap(), context_before_reopen);
            drop(reopened);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn failed_summary_does_not_change_the_context_projection() {
        let root = std::env::temp_dir().join(format!("ion-summary-fail-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let (turn, _) = session.begin_turn("task".into(), model()).unwrap();
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("done".into())],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        let before = session.context_messages().unwrap();
        let scripts = Arc::new(ScriptedModelService::new((0..3).map(|_| {
            Script::OpenError(ProviderError {
                kind: ProviderErrorKind::Server,
                message: "unavailable".into(),
                retry_after_ms: None,
            })
        })));
        let agent = Agent::new(scripts, Arc::new(TestTools::new(&root)), model());
        assert!(
            agent
                .compact(session.clone(), CancellationToken::new(), |_| {})
                .await
                .is_err()
        );
        assert_eq!(session.context_messages().unwrap(), before);
        assert_eq!(session.view().unwrap().compacted_through, None);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn invalid_summary_reports_cause_without_changing_committed_context() {
        let cases = [
            (
                ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
                Role::Assistant,
                vec![Content::Text("partial".into())],
                "model output limit reached",
            ),
            (
                ResponseTermination::Incomplete(IncompleteReason::ContextLength),
                Role::Assistant,
                vec![Content::Text("partial".into())],
                "model context limit reached",
            ),
            (
                ResponseTermination::Incomplete(IncompleteReason::ContentFilter),
                Role::Assistant,
                vec![Content::Text("partial".into())],
                "provider content filter interrupted response",
            ),
            (
                ResponseTermination::Incomplete(IncompleteReason::Other(
                    "untrusted\u{001b}provider detail".into(),
                )),
                Role::Assistant,
                vec![Content::Text("partial".into())],
                "provider ended response before completion",
            ),
            (
                ResponseTermination::Completed,
                Role::User,
                vec![Content::Text("not an assistant".into())],
                "response is not an assistant message",
            ),
            (
                ResponseTermination::Completed,
                Role::Assistant,
                vec![Content::ToolCall(ToolCall {
                    id: "invalid-summary-call".into(),
                    name: "write".into(),
                    arguments: serde_json::json!({"path":"unexpected","content":"effect"}),
                    raw_arguments: None,
                })],
                "response contains non-text content",
            ),
            (
                ResponseTermination::Completed,
                Role::Assistant,
                vec![
                    Content::Thinking("not summary text".into()),
                    Content::Text(" \n".into()),
                ],
                "response contains no summary text",
            ),
        ];
        for (termination, role, content, reason) in cases {
            let root =
                std::env::temp_dir().join(format!("ion-summary-invalid-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&root).unwrap();
            let session =
                Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
            let (turn, _) = session.begin_turn("task".into(), model()).unwrap();
            session
                .record_assistant(
                    turn,
                    Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("done".into())],
                        provider_replay: None,
                    },
                    Usage::unknown(),
                    false,
                )
                .unwrap();
            let before = session.view().unwrap();
            let context = session.context_messages().unwrap();
            let scripts = Arc::new(ScriptedModelService::new([Script::Stream(vec![
                ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role,
                        content,
                        provider_replay: None,
                    },
                    usage: Usage::unknown(),
                    termination,
                    returned_model: None,
                }),
            ])]));
            let agent = Agent::new(scripts, Arc::new(TestTools::new(&root)), model());
            let error = agent
                .compact(session.clone(), CancellationToken::new(), |_| {})
                .await
                .unwrap_err();
            assert!(matches!(error, AgentError::InvalidSummary(actual) if actual == reason));
            assert_eq!(
                error.to_string(),
                format!("model returned an invalid context summary: {reason}")
            );
            assert_eq!(
                error.end_reason(),
                TurnEndReason::Failed(format!("invalid_summary: {reason}"))
            );
            assert_eq!(session.context_messages().unwrap(), context);
            assert_eq!(session.view().unwrap().entries, before.entries);
            session.shutdown().await.unwrap();
            drop(session);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn cancelled_summary_does_not_change_the_context_projection() {
        struct CancelOnCompletion(CancellationToken);

        impl ModelService for CancelOnCompletion {
            fn stream<'a>(
                &'a self,
                _request: ModelRequest,
            ) -> BoxFuture<'a, Result<ion_ai::ModelStream, ProviderError>> {
                let stop = self.0.clone();
                Box::pin(async move {
                    let events = futures_util::stream::once(async move {
                        stop.cancel();
                        Ok(ModelStreamEvent::Completed(ModelResponse {
                            message: Message {
                                role: Role::Assistant,
                                content: vec![Content::Text("valid summary".into())],
                                provider_replay: None,
                            },
                            usage: Usage::unknown(),
                            termination: ResponseTermination::Completed,
                            returned_model: Some("test".into()),
                        }))
                    });
                    Ok(Box::pin(events) as ion_ai::ModelStream)
                })
            }
        }

        let root =
            std::env::temp_dir().join(format!("ion-summary-cancel-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let (turn, _) = session.begin_turn("task".into(), model()).unwrap();
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("done".into())],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        let before = session.view().unwrap();
        let stop = CancellationToken::new();
        let agent = Agent::new(
            Arc::new(CancelOnCompletion(stop.clone())),
            Arc::new(TestTools::new(&root)),
            model(),
        );
        assert!(matches!(
            agent.compact(session.clone(), stop, |_| {}).await,
            Err(AgentError::Cancelled)
        ));
        let after = session.view().unwrap();
        assert_eq!(after.entries, before.entries);
        assert_eq!(after.compacted_through, None);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn request_pressure_compacts_before_the_next_model_request() {
        let root = std::env::temp_dir().join(format!("ion-pressure-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("x".repeat(2_200))]),
            response(vec![Content::Text("First task completed.".into())]),
            response(vec![Content::Text("continued".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model())
            .with_limits(AgentLimits {
                // Explicit retained-answer pressure, not verbose native-tool
                // descriptions in this minimal protocol fixture.
                max_request_bytes: 3_200,
                ..AgentLimits::default()
            });
        for prompt in ["first", "second"] {
            agent
                .submit(
                    session.clone(),
                    prompt.into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap();
        }
        assert_eq!(scripts.requests().len(), 3);
        assert!(scripts.requests()[1].tools.is_empty());
        assert!(session.view().unwrap().compacted_through.is_some());
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn compaction_uses_available_capacity_without_skipping_valid_settled_prefixes() {
        for (window, lengths) in [(12_000, vec![10]), (50_000, vec![49_900, 13_400, 34_900])] {
            let root =
                std::env::temp_dir().join(format!("ion-summary-capacity-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&root).unwrap();
            let path = root.join("session.sqlite");
            let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
            let seed_service =
                Arc::new(ScriptedModelService::new(lengths.iter().map(|&length| {
                    response(vec![Content::Text("b".repeat(length))])
                })));
            let seed_agent = Agent::new(seed_service, Arc::new(TestTools::new(&root)), model())
                .with_limits(AgentLimits {
                    max_output_tokens: 100_000,
                    ..AgentLimits::default()
                });
            for length in lengths {
                seed_agent
                    .submit(
                        session.clone(),
                        "a".repeat(length),
                        "test".into(),
                        CancellationToken::new(),
                        |_| {},
                    )
                    .await
                    .unwrap();
            }
            let before = session.view().unwrap().entries;
            let scripts =
                Arc::new(ScriptedModelService::new((0..3).map(|_| {
                    response(vec![Content::Text("Retained settled history.".into())])
                })));
            let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model())
                .with_limits(AgentLimits {
                    context_window_tokens: Some(window),
                    ..AgentLimits::default()
                });
            let cuts = Arc::new(Mutex::new(Vec::new()));
            let observed_cuts = cuts.clone();
            let changed = agent
                .compact(session.clone(), CancellationToken::new(), move |event| {
                    if let AgentEvent::ContextCompacted { through_entry } = event {
                        observed_cuts.lock().unwrap().push(through_entry);
                    }
                })
                .await;
            let cuts = cuts.lock().unwrap();
            let after = session.view().unwrap().entries;
            drop(session);
            let reopened = Arc::new(CodingSession::open(&path).unwrap());
            let reopened_entries = reopened.view().unwrap().entries;
            drop(reopened);
            std::fs::remove_dir_all(root).unwrap();
            assert!(changed.unwrap(), "window {window}");
            assert!(after.starts_with(&before));
            assert_eq!(reopened_entries, after);
            let requests = scripts.requests();
            if window == 12_000 {
                assert_eq!(cuts.len(), 1);
                assert!(requests[0].controls.max_output_tokens > 0);
                assert!(requests[0].controls.max_output_tokens < 4096);
            } else {
                assert_eq!(cuts.len(), 2);
                assert!(cuts[0] < cuts[1]);
                assert_eq!(requests.len(), 2);
                assert_eq!(
                    requests[0].messages[0].content.len(),
                    2,
                    "the first settled Turn must remain a candidate"
                );
            }
            for request in requests {
                assert_eq!(request.route.reason, ModelRouteReason::Auxiliary);
                assert!(request.tools.is_empty());
                assert_eq!(
                    agent.limits.request_output_budget(&request, 4096).unwrap(),
                    Some(request.controls.max_output_tokens)
                );
            }
        }
    }

    #[tokio::test]
    async fn long_saved_history_compacts_in_bounded_steps() {
        let root = std::env::temp_dir().join(format!("ion-long-context-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        for index in 0..30 {
            let (turn, _) = session
                .begin_turn(format!("task {index}: {}", "a".repeat(240)), model())
                .unwrap();
            session
                .record_assistant(
                    turn,
                    Message {
                        role: Role::Assistant,
                        content: vec![Content::Text(format!(
                            "observed task {index}: {}",
                            "b".repeat(240)
                        ))],
                        provider_replay: None,
                    },
                    Usage::unknown(),
                    false,
                )
                .unwrap();
        }
        let raw_before = session.view().unwrap().entries.len();
        let scripts = Arc::new(ScriptedModelService::new((0..40).map(|index| {
            response(vec![Content::Text(format!(
                "Summary through chunk {index}"
            ))])
        })));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model())
            .with_limits(AgentLimits {
                max_request_bytes: 2_000,
                ..AgentLimits::default()
            });
        let cuts = Arc::new(Mutex::new(Vec::new()));
        let observed_cuts = cuts.clone();
        assert!(
            agent
                .compact(session.clone(), CancellationToken::new(), move |event| {
                    if let AgentEvent::ContextCompacted { through_entry } = event {
                        observed_cuts.lock().unwrap().push(through_entry);
                    }
                })
                .await
                .unwrap()
        );
        let cuts = cuts.lock().unwrap();
        assert!(cuts.len() > 1, "expected incremental summaries: {cuts:?}");
        assert!(cuts.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            serde_json::to_vec(&session.context_messages().unwrap())
                .unwrap()
                .len()
                <= 1_000
        );
        assert_eq!(
            session.view().unwrap().entries.len(),
            raw_before + cuts.len()
        );
        assert!(scripts.requests().iter().all(|request| {
            request.tools.is_empty() && serde_json::to_vec(request).unwrap().len() <= 2_000
        }));
        drop(session);
        let reopened = Arc::new(CodingSession::open(&path).unwrap());
        assert_eq!(
            reopened.view().unwrap().compacted_through,
            cuts.last().copied()
        );
        assert_eq!(
            reopened.view().unwrap().entries.len(),
            raw_before + cuts.len()
        );
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn oversized_input_is_rejected_before_turn_admission() {
        let root = std::env::temp_dir().join(format!("ion-input-bound-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([response(vec![Content::Text(
            "small input accepted".into(),
        )])]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model())
            .with_limits(AgentLimits {
                max_request_bytes: 2_048,
                ..AgentLimits::default()
            });
        for prompt in ["x".repeat(2_049), "\0".repeat(400)] {
            let accepted = Arc::new(Mutex::new(false));
            let observed_accepted = accepted.clone();
            let result = agent
                .submit(
                    session.clone(),
                    prompt,
                    String::new(),
                    CancellationToken::new(),
                    move |event| {
                        *observed_accepted.lock().unwrap() |=
                            matches!(event, AgentEvent::TurnAccepted { .. });
                    },
                )
                .await;
            assert!(matches!(
                result,
                Err(AgentError::InputTooLarge {
                    max_encoded_bytes: 2_048
                })
            ));
            assert!(!*accepted.lock().unwrap());
            assert!(session.view().unwrap().entries.is_empty());
            assert!(scripts.requests().is_empty());
        }
        assert_eq!(
            agent
                .submit(
                    session.clone(),
                    "small".into(),
                    String::new(),
                    CancellationToken::new(),
                    |_| {}
                )
                .await
                .unwrap(),
            "small input accepted"
        );
        assert_eq!(scripts.requests().len(), 1);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn oversized_atomic_turn_reports_capacity_without_changing_history() {
        let root =
            std::env::temp_dir().join(format!("ion-atomic-context-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let (turn, _) = session.begin_turn("x".repeat(4_000), model()).unwrap();
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("done".into())],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        let before = session.view().unwrap().entries;
        let scripts = Arc::new(ScriptedModelService::new([]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model())
            .with_limits(AgentLimits {
                max_request_bytes: 2_000,
                ..AgentLimits::default()
            });
        assert!(matches!(
            agent
                .compact(session.clone(), CancellationToken::new(), |_| {})
                .await,
            Err(AgentError::ContextTooLarge)
        ));
        assert!(scripts.requests().is_empty());
        assert_eq!(session.view().unwrap().entries, before);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn visible_partial_output_is_not_silently_replayed_after_overflow() {
        let root = std::env::temp_dir().join(format!("ion-partial-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("first result".into())]),
            Script::Stream(vec![
                ModelStreamEvent::TextDelta("partial".into()),
                ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("partial".into())],
                        provider_replay: None,
                    },
                    usage: Usage::unknown(),
                    termination: ResponseTermination::Incomplete(IncompleteReason::ContextLength),
                    returned_model: Some("test".into()),
                }),
            ]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(TestTools::new(&root)), model());
        agent
            .submit(
                session.clone(),
                "first".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert!(
            agent
                .submit(
                    session.clone(),
                    "second".into(),
                    "test".into(),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .is_err()
        );
        assert_eq!(scripts.requests().len(), 2);
        assert_eq!(session.view().unwrap().compacted_through, None);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn incomplete_tool_call_is_reported_without_reexecution() {
        let root =
            std::env::temp_dir().join(format!("ion-agent-interrupt-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let (turn, _) = session.begin_turn("first".into(), model()).unwrap();
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(ToolCall {
                        id: "missing".into(),
                        name: "write".into(),
                        arguments: serde_json::json!({"path":"should-not-exist","content":"wrong"}),
                        raw_arguments: None,
                    })],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        drop(session);
        let reopened = Arc::new(CodingSession::open(&path).unwrap());
        let scripts = Arc::new(ScriptedModelService::new([response(vec![Content::Text(
            "inspected".into(),
        )])]));
        let agent = Agent::new(scripts, Arc::new(TestTools::new(&root)), model());
        let interrupted = Arc::new(Mutex::new(0));
        let observed_interrupted = interrupted.clone();
        agent
            .submit(
                reopened.clone(),
                "inspect first".into(),
                "test".into(),
                CancellationToken::new(),
                move |event| {
                    if let AgentEvent::InterruptedCalls(count) = event {
                        *observed_interrupted.lock().unwrap() = count;
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(*interrupted.lock().unwrap(), 1);
        assert!(!root.join("should-not-exist").exists());
        assert!(matches!(
            &reopened.messages().unwrap()[1].content[0],
            Content::ToolCall(_)
        ));
        assert!(matches!(
            &reopened.messages().unwrap()[2].content[0],
            Content::ToolResult(_)
        ));
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }
}
