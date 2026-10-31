//! One committed, ordered batch. Its non-dispatch proof is live custody, never
//! reconstructed from a missing observation after process loss.
use super::{
    Agent, AgentError, AgentEvent, PromptCacheWarmingPolicy,
    cache_warming::{PromptCacheWarmer, execute_with_cache_warming},
};
use crate::{
    request::PreparedRequest,
    session::{ModelContextSnapshot, Session},
    tool_set::{ToolDispatch, ToolExecution},
};
use ion_ai::ToolCall;
use std::{collections::BTreeSet, sync::Arc};
use tokio_util::sync::CancellationToken;

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
