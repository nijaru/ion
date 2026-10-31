//! One admitted Turn: origin publication, phase sequencing and final closure.
use super::{
    Agent, AgentError, AgentEvent, SteeringInbox,
    effects::OrderedBatch,
    inbox::CommittedInputs,
    recovery::{TurnOrigin, TurnStart},
    request_cycle::RequestCycle,
};
use crate::{
    generation::GeneratedResponse,
    request::RequestStep,
    session::{Session, StoredToolActivity, TurnEndReason},
    tool_set::ToolActivity,
};
use ion_ai::{
    Content, IncompleteReason, ModelExecution, ModelRouteReason, ResponseTermination, Role,
};
use std::{collections::BTreeSet, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(super) struct Turn<'a> {
    pub agent: &'a Agent,
    pub session: &'a Arc<Session>,
    pub start: TurnStart,
    pub instructions: String,
    pub stop: &'a CancellationToken,
}

impl Turn<'_> {
    pub async fn run<F, M: Send>(
        self,
        steering: Option<&SteeringInbox<M>>,
        observe: &mut F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let Self {
            agent,
            session,
            start,
            instructions,
            stop,
        } = self;
        let turn = start.turn;
        let mut reason = match start.origin {
            TurnOrigin::User { entry, input } => {
                observe(AgentEvent::TurnAccepted { turn, entry, input });
                ModelRouteReason::UserRequest
            }
            TurnOrigin::Resumed { point } => {
                observe(AgentEvent::TurnResumed { point });
                ModelRouteReason::Recovery
            }
            TurnOrigin::Automated { entry, notices } => {
                observe(AgentEvent::CompletionCommitted {
                    turn,
                    entry,
                    notices,
                    new_turn: true,
                });
                ModelRouteReason::DelegationCompletion
            }
        };
        if start.interrupted > 0 {
            observe(AgentEvent::InterruptedCalls(start.interrupted));
        }
        let mut cycle = RequestCycle::new(agent, session, turn, &instructions, stop, start.catalog);
        let mut batch = None;
        let outcome = async {
            loop {
                let RequestStep {
                    generated: GeneratedResponse { response, route },
                    prepared,
                    started,
                } = cycle.next(reason, steering, observe).await?;
                let catalog = prepared.catalog();
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
                        activity: catalog.activity(call),
                    })
                    .collect::<Vec<_>>();
                let committed_content = response.message.content.clone();
                if truncated_calls {
                    let (entry, results) = session.record_truncated_assistant(
                        turn,
                        response.message,
                        tool_activities.clone(),
                        execution.clone(),
                        response.usage,
                    )?;
                    observe(AgentEvent::AssistantCommitted {
                        turn,
                        entry,
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
                                |call| catalog.activity(call),
                            );
                        observe(AgentEvent::ToolFinished {
                            call_id: result.call_id,
                            name: result.name,
                            activity,
                            outcome: result.outcome,
                        });
                    }
                    prepared.record_observed_input(session, &execution, response.usage)?;
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
                // Assistant plus ready boundary input commits atomically. Later
                // arrivals remain in the inbox for the next request boundary.
                let (committed, committed_steering) = if calls.is_empty()
                    && let Some(inbox) = steering
                {
                    inbox.record_assistant(
                        session,
                        turn,
                        response.message,
                        tool_activities.clone(),
                        execution.clone(),
                        response.usage,
                        agent.limits,
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
                        CommittedInputs::default(),
                    )
                };
                observe(AgentEvent::AssistantCommitted {
                    turn,
                    entry: committed.entry,
                    content: committed_content,
                    tool_activities,
                    termination: response.termination,
                });
                let human_continuation = !committed_steering.steering.is_empty();
                committed_steering.observe(turn, observe);
                prepared.record_observed_input(session, &execution, response.usage)?;
                if committed.complete {
                    observe(AgentEvent::TurnEnded {
                        turn,
                        reason: TurnEndReason::Completed,
                    });
                    observe(AgentEvent::Final(final_text.clone()));
                    return Ok(final_text);
                }
                reason = if calls.is_empty() && human_continuation {
                    ModelRouteReason::Steering
                } else if calls.is_empty() {
                    ModelRouteReason::DelegationCompletion
                } else {
                    ModelRouteReason::ToolContinuation
                };
                if !calls.is_empty() {
                    batch
                        .insert(OrderedBatch::new(
                            calls,
                            committed.entry,
                            prepared,
                            agent.limits.prompt_cache_warming,
                            response.usage,
                            started,
                        ))
                        .run(agent, session, turn, &instructions, stop, observe)
                        .await?;
                    batch = None;
                }
            }
        }
        .await;
        match outcome {
            Ok(answer) => Ok(answer),
            Err(error) => {
                // Storage failure blocks dependency and is never disguised by a
                // second closure write. Only the live batch proves non-dispatch.
                if !matches!(error, AgentError::Session(_)) {
                    let reason = error.end_reason();
                    let empty = BTreeSet::new();
                    let never_dispatched = batch
                        .as_ref()
                        .map_or(&empty, OrderedBatch::never_dispatched);
                    for (result, activity) in
                        session.end_turn(turn, reason.clone(), never_dispatched)?
                    {
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
