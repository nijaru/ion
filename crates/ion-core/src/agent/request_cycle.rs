//! Request rebuild authority for one admitted Turn. Transient retries stay in
//! generation and retain the frozen request; rebuilds always return to intake.
use super::{Agent, AgentError, AgentEvent, SteeringInbox};
use crate::{
    generation::OutputObservation,
    request::{RequestPlan, RequestStep},
    session::{Session, SessionError},
    tool_set::ToolCatalog,
};
use ion_ai::{IncompleteReason, ModelResponse, ModelRouteReason, ResponseTermination};
use tokio_util::sync::CancellationToken;

pub(super) struct RequestCycle<'a> {
    agent: &'a Agent,
    session: &'a Session,
    turn: u64,
    instructions: &'a str,
    stop: &'a CancellationToken,
    catalog: Option<ToolCatalog>,
    length_recovery_attempted: bool,
    replay_rebased: bool,
}

impl<'a> RequestCycle<'a> {
    pub fn new(
        agent: &'a Agent,
        session: &'a Session,
        turn: u64,
        instructions: &'a str,
        stop: &'a CancellationToken,
        catalog: Option<ToolCatalog>,
    ) -> Self {
        Self {
            agent,
            session,
            turn,
            instructions,
            stop,
            catalog,
            length_recovery_attempted: false,
            replay_rebased: false,
        }
    }

    pub async fn next<F, M: Send>(
        &mut self,
        reason: ModelRouteReason,
        steering: Option<&SteeringInbox<M>>,
        observe: &mut F,
    ) -> Result<RequestStep, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let mut recovered_overflow = false;
        let mut reserve_handled = false;
        loop {
            // Yield even for an entirely buffered service/tool sequence.
            tokio::task::yield_now().await;
            if self.stop.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            if self.catalog.is_none() {
                for diagnostic in self.agent.tools.refresh(self.stop.clone()).await {
                    observe(AgentEvent::ToolCatalogWarning(diagnostic));
                }
            }
            if self.stop.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            if let Some(inbox) = steering {
                inbox
                    .record_pending(self.session, self.turn, self.agent.limits)?
                    .observe(self.turn, observe);
            }
            if self.stop.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let basis = self.session.request_basis(&self.agent.model)?;
            if basis.turn != Some(self.turn) {
                return Err(SessionError::InvalidHistory.into());
            }
            let assistant_seen = basis.assistant_seen;
            let prefix_bound = basis.prefix_bound;
            let plan = RequestPlan::new(
                self.session,
                basis,
                &self.agent.service,
                &self.agent.tools,
                reason,
                self.agent.request_settings(self.instructions),
                self.catalog.as_ref(),
            )?;
            let output_budget = plan.output_budget();
            let required = output_budget.is_none();
            let keep_bytes = plan.keep_bytes();
            // Required and optional compaction share the same measured target.
            // A size-bounded partial cut may still need the remaining prefix;
            // a complete cut must not trigger a redundant optional summary.
            if !prefix_bound
                && (required
                    || (!reserve_handled
                        && output_budget.is_some_and(|budget| budget < plan.output_reserve())))
            {
                // Every outcome rebuilds or exits. Do not retain this unused
                // request while the summary builds another context view.
                drop(plan);
                if !required {
                    reserve_handled = true;
                }
                match self
                    .agent
                    .compact_inner(
                        None,
                        self.session,
                        self.stop,
                        keep_bytes,
                        !required,
                        observe,
                    )
                    .await
                {
                    Ok(Some(chunked)) => {
                        if !chunked {
                            reserve_handled = true;
                        }
                        continue;
                    }
                    // Even a discarded summary may have awaited I/O. Return
                    // through intake for newly queued steering/stop.
                    Ok(None) | Err(AgentError::ContextTooLarge) if !required => continue,
                    Ok(None) => return Err(AgentError::ContextTooLarge),
                    Err(error) => return Err(error),
                }
            }
            let output_budget = output_budget.ok_or(AgentError::ContextTooLarge)?;
            let issued = plan
                .issue(self.session, &self.agent.service, self.stop, observe)
                .await?;
            let attempt = issued.attempt;
            if !assistant_seen
                && !self.replay_rebased
                && matches!(&attempt.outcome, Err(AgentError::ReplayContextChanged))
            {
                self.session.rebase_provider_replay(self.turn)?;
                observe(AgentEvent::ProviderReplayRebased);
                self.replay_rebased = true;
                continue;
            }
            let overflow = attempt.context_length_exceeded();
            let recoverable_length = !self.length_recovery_attempted
                && matches!(
                    &attempt.outcome,
                    Ok(ModelResponse {
                        termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
                        usage: ion_ai::Usage { output_tokens: Some(output), .. },
                        ..
                    }) if *output < u64::from(output_budget)
                );
            if ((overflow && attempt.output == OutputObservation::Unobserved) || recoverable_length)
                && !prefix_bound
                && !recovered_overflow
            {
                match self
                    .agent
                    .compact_inner(None, self.session, self.stop, keep_bytes, false, observe)
                    .await
                {
                    Ok(None) => {}
                    Err(error @ AgentError::Session(_)) => return Err(error),
                    recovery => {
                        // Failed streams were accounted by generation. A completed
                        // response is discarded only if recovery replaces it (or
                        // fails); otherwise the Turn still owns its disposition.
                        if let Some(observed) = &attempt.usage {
                            self.session.record_uncommitted_usage(
                                crate::session::ModelUsagePurpose::Assistant,
                                observed.execution.clone(),
                                observed.usage,
                            )?;
                        }
                        let _ = recovery?;
                        recovered_overflow = true;
                        if recoverable_length {
                            self.length_recovery_attempted = true;
                            observe(AgentEvent::ResponseRestarted);
                        }
                        continue;
                    }
                }
            }
            let started = attempt.timing.start;
            return Ok(RequestStep {
                generated: attempt.into_result()?,
                plan: issued.plan,
                started,
            });
        }
    }
}
