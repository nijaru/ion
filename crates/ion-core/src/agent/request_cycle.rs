//! Request rebuild authority for one admitted Turn. Transient retries stay in
//! generation and retain the frozen request; rebuilds always return to intake.
use super::{Agent, AgentError, AgentEvent, SteeringInbox};
use crate::{
    generation::OutputCustody,
    request::{PreparedRequest, RequestStep},
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
            let prepared = match PreparedRequest::new(
                self.session,
                basis,
                &self.agent.service,
                &self.agent.tools,
                reason,
                self.agent.request_settings(self.instructions),
                self.catalog.as_ref(),
            ) {
                Ok(prepared) => prepared,
                Err(AgentError::ContextTooLarge) if !prefix_bound => {
                    if self
                        .agent
                        .compact_inner(
                            None,
                            self.session,
                            self.stop,
                            self.agent.keep_bytes(),
                            observe,
                        )
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
            let issued = prepared
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
            if ((overflow && attempt.output == OutputCustody::Unobserved) || recoverable_length)
                && !prefix_bound
                && !recovered_overflow
                && self
                    .agent
                    .compact_inner(
                        None,
                        self.session,
                        self.stop,
                        self.agent.keep_bytes(),
                        observe,
                    )
                    .await?
                    .is_some()
            {
                recovered_overflow = true;
                if recoverable_length {
                    self.length_recovery_attempted = true;
                    observe(AgentEvent::ResponseRestarted);
                }
                continue;
            }
            let started = attempt.timing.start;
            return Ok(RequestStep {
                generated: attempt.into_result()?,
                prepared: issued.prepared,
                started,
            });
        }
    }
}
