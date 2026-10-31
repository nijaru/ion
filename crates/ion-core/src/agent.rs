//! Bound agent configuration and admission into the shared Turn engine.
#[cfg(test)]
use crate::generation::generate_with_retry;
use ion_ai::{
    Content, Message, ModelRef, ModelService, ProviderError, Reasoning, ResponseTermination, Role,
};
#[cfg(test)]
use ion_ai::{
    GenerationControls, IncompleteReason, ModelExecution, ModelRequest, ModelResponse, ModelRoute,
    ModelRouteReason, ProviderErrorKind, ToolChoice,
};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    RequestReceipt, SubmissionRequest,
    session::{Session, SessionError, StoredToolActivity, TurnAdmission, TurnEndReason},
    tool_set::{ToolActivity, ToolSet, ToolSource},
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

mod cache_warming;
pub use cache_warming::PromptCacheWarmingPolicy;
mod completion;
pub use completion::CompletionSubmission;
mod inbox;
pub use inbox::SteeringInbox;

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

mod compaction;
mod recovery;
#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod summary_recovery_tests;
use recovery::{TurnOrigin, TurnStart};
mod effects;
mod request_cycle;
mod turn;

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
            let (turn, entry, interrupted, input) = match admission {
                TurnAdmission::Existing(receipt) => return Ok((receipt, None)),
                TurnAdmission::New {
                    turn,
                    entry,
                    interrupted,
                    input,
                } => (turn, entry, interrupted, input),
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
                                origin: TurnOrigin::User { entry, input },
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
        let TurnAdmission::New {
            turn,
            entry,
            interrupted,
            input,
        } = session.admit_turn(input, self.model.clone(), None, binding)?
        else {
            unreachable!("anonymous input cannot reconcile");
        };
        self.run_admitted(
            session,
            TurnStart {
                turn,
                interrupted,
                origin: TurnOrigin::User { entry, input },
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
        turn::Turn {
            agent: self,
            session,
            start,
            instructions,
            stop: &stop,
        }
        .run(steering, observe)
        .await
    }
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Continuation, not another input acceptance. History must be reconciled.
    TurnResumed {
        point: crate::RecoveryPoint,
    },
    /// Exact normalized human input, published only after admission commits.
    TurnAccepted {
        turn: u64,
        /// Durable global Session entry coordinate.
        entry: u64,
        input: Message,
    },
    /// A durable automated context offer, never a human input acceptance.
    CompletionCommitted {
        turn: u64,
        /// TurnStarted for a new automated Turn; Completion otherwise.
        entry: u64,
        notices: Vec<crate::CompletionNotice>,
        new_turn: bool,
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
        /// Durable global Session entry coordinate.
        entry: u64,
        content: Vec<Content>,
        tool_activities: Vec<StoredToolActivity>,
        termination: ResponseTermination,
    },
    /// Published after steering enters the Session, in durable order.
    SteeringCommitted {
        turn: u64,
        /// Durable global Session entry coordinate.
        entry: u64,
        input: Message,
    },
    ProviderRetry {
        attempt: usize,
        max_retries: usize,
        delay_ms: u64,
    },
    /// A pre-output summary capacity rejection is retried at a strictly smaller
    /// settled cut. Progress only: the existing context has not been replaced.
    ContextCompactionRetry {
        through_entry: u64,
        next_through_entry: u64,
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
mod tests;
