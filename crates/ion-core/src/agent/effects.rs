//! One committed, ordered batch. Its non-dispatch proof is live custody, never
//! reconstructed from a missing observation after process loss.
use super::{Agent, AgentError, AgentEvent, PromptCacheWarmingPolicy};
use crate::{
    generation::refresh_prompt_cache,
    request::PreparedRequest,
    session::{ModelContextSnapshot, Session},
    tool_set::{ToolDispatch, ToolExecution},
};
use ion_ai::{ModelRequest, ModelService, ToolCall};
use std::{collections::BTreeSet, sync::Arc};
use tokio_util::sync::CancellationToken;

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

pub(super) struct OrderedBatch {
    calls: Vec<ToolCall>,
    entry: u64,
    prepared: PreparedRequest,
    warmer: Option<PromptCacheWarmer>,
    never_dispatched: BTreeSet<String>,
}

impl OrderedBatch {
    pub fn new(
        calls: Vec<ToolCall>,
        entry: u64,
        prepared: PreparedRequest,
        policy: Option<PromptCacheWarmingPolicy>,
        usage: ion_ai::Usage,
        started: tokio::time::Instant,
    ) -> Self {
        let warmer = policy
            .and_then(|policy| PromptCacheWarmer::new(policy, prepared.request(), usage, started));
        Self {
            never_dispatched: calls.iter().map(|call| call.id.clone()).collect(),
            calls,
            entry,
            prepared,
            warmer,
        }
    }

    pub fn never_dispatched(&self) -> &BTreeSet<String> {
        &self.never_dispatched
    }

    pub async fn run<F>(
        &mut self,
        agent: &Agent,
        session: &Arc<Session>,
        turn: u64,
        instructions: &str,
        stop: &CancellationToken,
        observe: &mut F,
    ) -> Result<(), AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let catalog = self.prepared.catalog();
        let mut activate_tools = BTreeSet::new();
        for (index, call) in self.calls.iter().enumerate() {
            if stop.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let activity = catalog.activity(call);
            let occurrence = crate::ToolOccurrence {
                assistant_entry: self.entry,
                ordinal: index,
            };
            let (invocation, gateway) = crate::ToolInvocation::admitted(
                call,
                stop.clone(),
                session.id(),
                turn,
                occurrence,
                None,
                self.prepared.reasoning(),
            );
            let invocation = invocation.with_model_messages(&self.prepared.request().messages);
            let tool = Box::pin(async {
                if stop.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                let dispatch = catalog.execute_model_call(invocation).await;
                if !matches!(dispatch, ToolDispatch::Rejected(_)) {
                    self.never_dispatched.remove(&call.id);
                    observe(AgentEvent::ToolStarted {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                        activity: activity.clone(),
                    });
                }
                match dispatch {
                    ToolDispatch::History => Ok(ToolExecution::output(
                        crate::session::history::execute(session, call, stop.clone()).await,
                    )),
                    ToolDispatch::Composition(runtime, limits) => crate::code_gateway::run(
                        runtime,
                        limits,
                        session,
                        turn,
                        catalog,
                        call,
                        occurrence,
                        self.prepared.reasoning(),
                        &self.prepared.request().messages,
                        stop,
                        observe,
                    )
                    .await
                    .map(ToolExecution::output),
                    ToolDispatch::Execution(execution) => Ok(execution.await),
                    ToolDispatch::Rejected(reason) => Ok(ToolExecution::NotDispatched { reason }),
                }
            });
            let tool = Box::pin(async { gateway.run(session, stop, tool).await? });
            let execution = execute_with_cache_warming(
                &agent.service,
                session,
                turn,
                tool,
                stop,
                &mut self.warmer,
            )
            .await?;
            let (outcome, activated) = match execution {
                ToolExecution::NotDispatched { reason } => {
                    (crate::ToolOutcome::NotDispatched { reason }, Vec::new())
                }
                ToolExecution::Observed { output, activate } => {
                    let projection = output.model_projection(
                        agent.limits.image_input,
                        agent.limits.max_request_bytes,
                    )?;
                    (
                        crate::ToolOutcome::Observed { output, projection },
                        activate,
                    )
                }
            };
            activate_tools.extend(activated);
            let next_context =
                (index + 1 == self.calls.len() && !activate_tools.is_empty()).then(|| {
                    ModelContextSnapshot {
                        instructions: instructions.into(),
                        tools: catalog.declared_specs_with(&activate_tools),
                    }
                });
            session.record_tool_settlement_with_context(
                turn,
                crate::ToolSettlement {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    outcome: outcome.clone(),
                },
                next_context,
            )?;
            self.never_dispatched.remove(&call.id);
            observe(AgentEvent::ToolFinished {
                call_id: call.id.clone(),
                name: call.name.clone(),
                activity,
                outcome,
            });
        }
        Ok(())
    }
}

async fn execute_with_cache_warming(
    service: &Arc<dyn ModelService>,
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
            if started >= valid_until {
                return (started, None);
            }
            (started, refresh_prompt_cache(service, request, stop).await)
        };
        tokio::pin!(refresh);
        tokio::select! {
            output = &mut tool => return output,
            refreshed = &mut refresh => {
                let (started, Some((execution, usage))) = refreshed else {
                    *warmer = None;
                    continue;
                };
                // Optimization accounting cannot interrupt owned effects. The
                // next correctness-critical write still surfaces storage faults.
                let _ = session.record_cache_warm(turn, execution, usage);
                if !warmer.as_mut().is_some_and(|state| state.reschedule(usage, started)) {
                    *warmer = None;
                }
            }
            () = stop.cancelled() => {
                *warmer = None;
                // Stop joins the executor and delegation cleanup; never drop it.
                return tool.await;
            }
        }
    }
}
