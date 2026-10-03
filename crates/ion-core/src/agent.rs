//! One coding loop for library, headless and terminal clients.
use ion_ai::{
    Content, GenerationControls, IncompleteReason, Message, ModelRef, ModelRequest, ModelResponse,
    ModelService, ProviderError, ProviderErrorKind, Reasoning, ResponseTermination, Role,
    ToolChoice, ToolResult,
};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    generation::generate_with_retry,
    session::{
        ModelContextSnapshot, Session, SessionError, StoredToolActivity, TurnEndReason,
        valid_user_message,
    },
    tool_set::{ToolActivity, ToolHost, ToolOutput, ToolSet},
};

fn user_text(prompt: String) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(prompt)],
        provider_replay: None,
    }
}

/// Host-owned input waiting for a Session commit. A failed write leaves the
/// messages here so the host can return them to its editor after the Turn.
#[derive(Default)]
pub struct SteeringInbox {
    pending: Mutex<VecDeque<Message>>,
}

impl SteeringInbox {
    pub fn push(&self, prompt: String) {
        if !prompt.trim().is_empty() {
            self.pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push_back(user_text(prompt));
        }
    }

    pub fn push_message(&self, input: Message) -> Result<(), AgentError> {
        if !valid_user_message(&input) {
            return Err(AgentError::InvalidUserInput);
        }
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(input);
        Ok(())
    }

    /// Return inputs that never entered the Session. Call after the Turn
    /// future finishes; the host can restore them to its editor or queue.
    pub fn take_uncommitted(&self) -> Vec<Message> {
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
    ) -> Result<(), AgentError> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.is_empty() {
            return Ok(());
        }
        validate_steering(&pending, limits)?;
        session.record_steerings(turn, pending.iter().cloned().collect())?;
        pending.clear();
        Ok(())
    }

    fn record_assistant(
        &self,
        session: &Session,
        turn: u64,
        message: ion_ai::Message,
        tool_activities: Vec<StoredToolActivity>,
        usage: ion_ai::Usage,
        limits: AgentLimits,
    ) -> Result<bool, AgentError> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_steering(&pending, limits)?;
        let complete = session.record_assistant_with_steering(
            turn,
            message,
            tool_activities,
            usage,
            pending.iter().cloned().collect(),
        )?;
        pending.clear();
        Ok(complete)
    }
}

fn validate_steering(pending: &VecDeque<Message>, limits: AgentLimits) -> Result<(), AgentError> {
    for input in pending {
        if !valid_user_message(input) {
            return Err(AgentError::InvalidUserInput);
        }
        if !limits.image_input
            && input
                .content
                .iter()
                .any(|part| matches!(part, Content::Image(_)))
        {
            return Err(AgentError::ImagesUnsupported);
        }
        if serde_json::to_vec(input)?.len() > limits.max_request_bytes {
            return Err(AgentError::ContextTooLarge);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct AgentLimits {
    pub max_request_bytes: usize,
    pub max_output_tokens: u32,
    pub context_window_tokens: Option<u32>,
    pub image_input: bool,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: 8 * 1024 * 1024,
            max_output_tokens: 16_384,
            context_window_tokens: None,
            image_input: false,
        }
    }
}

pub struct Agent {
    model: Arc<dyn ModelService>,
    tools: Arc<ToolSet>,
    limits: AgentLimits,
}

impl Agent {
    pub fn new(model: Arc<dyn ModelService>, tools: Arc<dyn ToolHost>) -> Self {
        Self::with_tool_set(model, Arc::new(ToolSet::new([tools])))
    }

    pub fn with_tool_set(model: Arc<dyn ModelService>, tools: Arc<ToolSet>) -> Self {
        Self {
            model,
            tools,
            limits: AgentLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: AgentLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn tool_catalog(&self) -> crate::tool_set::ToolCatalog {
        self.tools.snapshot()
    }

    /// Summarize a settled prefix while retaining the complete raw Session.
    pub async fn compact<F>(
        &self,
        session: &Session,
        model: ModelRef,
        stop: CancellationToken,
        mut observe: F,
    ) -> Result<bool, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let _guard = tokio::select! {
            guard = session.submit_gate.lock() => guard,
            () = stop.cancelled() => return Err(AgentError::Cancelled),
        };
        let keep_bytes = self
            .keep_bytes()
            .min(serde_json::to_vec(&session.context_messages()?)?.len() / 2);
        let mut changed = false;
        loop {
            match self
                .compact_inner(session, &model, &stop, keep_bytes, &mut observe)
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

    fn output_budget(&self, bytes: usize, estimated_input: u64, ceiling: u32) -> Option<u32> {
        if bytes > self.limits.max_request_bytes || ceiling == 0 {
            return None;
        }
        let Some(window) = self.limits.context_window_tokens else {
            return Some(ceiling);
        };
        let available = u64::from(window).saturating_sub(estimated_input + 8_192);
        (available > 0).then(|| ceiling.min(available as u32))
    }

    fn request_footprint(request: &ModelRequest) -> Result<(usize, u64), serde_json::Error> {
        let bytes = serde_json::to_vec(request)?.len();
        let (encoded_images, image_count) = request
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .fold((0usize, 0u64), |(bytes, count), part| match part {
                Content::Image(image) => (bytes.saturating_add(image.data().len()), count + 1),
                Content::ToolResult(result) => result
                    .images
                    .iter()
                    .fold((bytes, count), |(bytes, count), image| {
                        (bytes.saturating_add(image.data().len()), count + 1)
                    }),
                _ => (bytes, count),
            });
        // Serialized base64 counts against the transport bound, but it is
        // not prompt text. Reserve a conservative visual-token allowance
        // per image until provider usage gives the observed count.
        let estimated_input =
            bytes.saturating_sub(encoded_images).div_ceil(3) as u64 + image_count * 16_384;
        Ok((bytes, estimated_input))
    }

    fn request_fits(
        &self,
        request: &ModelRequest,
        output_tokens: u32,
    ) -> Result<bool, serde_json::Error> {
        let (bytes, estimated_input) = Self::request_footprint(request)?;
        Ok(self.output_budget(bytes, estimated_input, output_tokens) == Some(output_tokens))
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
        model: &ModelRef,
        stop: &CancellationToken,
        keep_bytes: usize,
        observe: &mut F,
    ) -> Result<Option<bool>, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let output_tokens = self.limits.max_output_tokens.min(4096);
        let mut budget = self.limits.max_request_bytes;
        let (through_entry, chunked, request) = loop {
            if stop.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let Some(plan) = session.compaction_plan(keep_bytes, budget)? else {
                return if session.compaction_plan(keep_bytes, usize::MAX)?.is_some() {
                    Err(AgentError::ContextTooLarge)
                } else {
                    Ok(None)
                };
            };
            let mut transcript = plan.messages;
            for message in &mut transcript {
                message.provider_replay = None;
            }
            let request = ModelRequest {
                model: model.clone(),
                instructions: Some("Summarize the coding conversation for continued work. Preserve the user's goal and constraints, current file changes and test results, important tool findings, unresolved errors, and precise next steps. Distinguish observations from guesses. Return only the summary.".into()),
                messages: vec![ion_ai::Message {
                    role: Role::User,
                    content: vec![Content::Text(format!(
                        "Conversation to summarize (JSON messages):\n{}",
                        serde_json::to_string(&transcript)?
                    ))],
                    provider_replay: None,
                }],
                tools: Vec::new(),
                controls: GenerationControls {
                    max_output_tokens: output_tokens,
                    temperature: None,
                    top_p: None,
                    reasoning: Reasoning::ProviderDefault,
                    tool_choice: ToolChoice::None,
                    parallel_tool_calls: false,
                },
            };
            if self.request_fits(&request, output_tokens)? {
                break (plan.through_entry, plan.chunked, request);
            }
            budget /= 2;
            if budget == 0 {
                return Err(AgentError::ContextTooLarge);
            }
        };
        let response = generate_with_retry(&self.model, request, stop, &mut |_| {}).await?;
        if !matches!(response.termination, ResponseTermination::Completed)
            || response.message.role != Role::Assistant
            || response
                .message
                .content
                .iter()
                .any(|part| !matches!(part, Content::Text(_)))
        {
            return Err(AgentError::InvalidSummary);
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
            return Err(AgentError::InvalidSummary);
        }
        if stop.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        session.record_compaction(through_entry, summary, response.usage)?;
        observe(AgentEvent::ContextCompacted { through_entry });
        Ok(Some(chunked))
    }

    pub async fn submit<F>(
        &self,
        session: &Session,
        model: ModelRef,
        prompt: String,
        instructions: String,
        stop: CancellationToken,
        mut observe: F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        if prompt.trim().is_empty() {
            return Err(AgentError::EmptyPrompt);
        }
        self.submit_inner(
            session,
            model,
            user_text(prompt),
            instructions,
            stop,
            None,
            &mut observe,
        )
        .await
    }

    /// Submit ordered text and image parts through the same durable Turn loop.
    pub async fn submit_message<F>(
        &self,
        session: &Session,
        model: ModelRef,
        input: Message,
        instructions: String,
        stop: CancellationToken,
        mut observe: F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        self.submit_inner(
            session,
            model,
            input,
            instructions,
            stop,
            None,
            &mut observe,
        )
        .await
    }

    /// Accept user steering at model-step boundaries during an active turn.
    /// Prompts not committed when the Turn ends remain in the host-owned inbox.
    #[expect(
        clippy::too_many_arguments,
        reason = "explicit turn and input lifecycles"
    )]
    pub async fn submit_with_steering<F>(
        &self,
        session: &Session,
        model: ModelRef,
        prompt: String,
        instructions: String,
        stop: CancellationToken,
        steering: &SteeringInbox,
        mut observe: F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        if prompt.trim().is_empty() {
            return Err(AgentError::EmptyPrompt);
        }
        self.submit_inner(
            session,
            model,
            user_text(prompt),
            instructions,
            stop,
            Some(steering),
            &mut observe,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "explicit Turn input and steering"
    )]
    pub async fn submit_message_with_steering<F>(
        &self,
        session: &Session,
        model: ModelRef,
        input: Message,
        instructions: String,
        stop: CancellationToken,
        steering: &SteeringInbox,
        mut observe: F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        self.submit_inner(
            session,
            model,
            input,
            instructions,
            stop,
            Some(steering),
            &mut observe,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "explicit turn and input lifecycles"
    )]
    async fn submit_inner<F>(
        &self,
        session: &Session,
        model: ModelRef,
        input: Message,
        instructions: String,
        stop: CancellationToken,
        steering: Option<&SteeringInbox>,
        observe: &mut F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let _guard = tokio::select! {
            guard = session.submit_gate.lock() => guard,
            () = stop.cancelled() => return Err(AgentError::Cancelled),
        };
        if stop.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        if !self.limits.image_input
            && (input
                .content
                .iter()
                .any(|part| matches!(part, Content::Image(_)))
                || session.context_messages()?.iter().any(|message| {
                    message
                        .content
                        .iter()
                        .any(|part| matches!(part, Content::Image(_)))
                }))
        {
            return Err(AgentError::ImagesUnsupported);
        }
        if input
            .content
            .iter()
            .any(|part| matches!(part, Content::Image(_)))
            && serde_json::to_vec(&input)?.len() > self.limits.max_request_bytes
        {
            return Err(AgentError::ContextTooLarge);
        }
        let (turn, interrupted) = session.begin_turn_message(input, model.clone())?;
        observe(AgentEvent::TurnAccepted { turn });
        if interrupted > 0 {
            observe(AgentEvent::InterruptedCalls(interrupted));
        }
        let outcome = self
            .drive(session, turn, model, instructions, &stop, steering, observe)
            .await;
        match outcome {
            Ok(answer) => Ok(answer),
            Err(error) => {
                // A failed storage write may leave the effect boundary uncertain.
                // Do not issue another write to disguise that failure.
                if !matches!(error, AgentError::Session(_)) {
                    session.end_turn(turn, error.end_reason())?;
                }
                Err(error)
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "explicit turn and input lifecycles"
    )]
    async fn drive<F>(
        &self,
        session: &Session,
        turn: u64,
        model: ModelRef,
        instructions: String,
        stop: &CancellationToken,
        steering: Option<&SteeringInbox>,
        observe: &mut F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let mut length_recovery_attempted = false;
        let mut assistant_seen_in_turn = false;
        let mut prefix_bound_continuation = false;
        let mut replay_rebased = false;
        loop {
            // Keep a fast in-process model from starving terminal input and
            // cancellation during a long tool sequence.
            tokio::task::yield_now().await;
            if stop.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            if let Some(inbox) = steering {
                inbox.record_pending(session, turn, self.limits)?;
            }
            let mut recovered_overflow = false;
            let (response, tool_catalog) = loop {
                for diagnostic in self.tools.refresh(stop.clone()).await {
                    observe(AgentEvent::ToolCatalogWarning(diagnostic));
                }
                if stop.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                let tool_catalog = self.tools.snapshot();
                let declared_tools = tool_catalog.specs();
                let mut request = ModelRequest {
                    model: model.clone(),
                    instructions: Some(instructions.clone()),
                    messages: session.context_messages_for(&model)?,
                    tools: declared_tools.clone(),
                    controls: GenerationControls {
                        max_output_tokens: self.limits.max_output_tokens,
                        temperature: None,
                        top_p: None,
                        reasoning: Reasoning::ProviderDefault,
                        tool_choice: ToolChoice::Auto,
                        parallel_tool_calls: true,
                    },
                };
                if !self.limits.image_input
                    && request.messages.iter().flat_map(|message| &message.content).any(|part| {
                        matches!(part, Content::Image(_))
                            || matches!(part, Content::ToolResult(result) if !result.images.is_empty())
                    })
                {
                    return Err(AgentError::ImagesUnsupported);
                }
                let (request_bytes, estimated_input) = Self::request_footprint(&request)?;
                let Some(output_budget) = self.output_budget(
                    request_bytes,
                    estimated_input,
                    self.limits.max_output_tokens,
                ) else {
                    if prefix_bound_continuation {
                        return Err(AgentError::ContextTooLarge);
                    }
                    if self
                        .compact_inner(session, &model, stop, self.keep_bytes(), observe)
                        .await?
                        .is_some()
                    {
                        continue;
                    }
                    return Err(AgentError::ContextTooLarge);
                };
                request.controls.max_output_tokens = output_budget;
                session.record_model_context(
                    turn,
                    ModelContextSnapshot {
                        instructions: instructions.clone(),
                        tools: declared_tools,
                    },
                )?;
                let mut emitted_text = false;
                let generated = generate_with_retry(&self.model, request, stop, &mut |event| {
                    if matches!(event, AgentEvent::TextDelta(_)) {
                        emitted_text = true;
                    }
                    observe(event);
                })
                .await;
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
                    Ok(ModelResponse {
                        termination: ResponseTermination::Incomplete(
                            IncompleteReason::ContextLength
                        ),
                        ..
                    })
                ) || (model.provider == "xiaomi"
                    && matches!(
                        &generated,
                        Ok(ModelResponse {
                            termination: ResponseTermination::Incomplete(
                                IncompleteReason::MaxOutputTokens
                            ),
                            usage: ion_ai::Usage {
                                output_tokens: Some(0),
                                input_tokens: Some(input),
                            },
                            ..
                        }) if self.limits.context_window_tokens.is_some_and(|window| {
                            *input * 100 >= u64::from(window) * 99
                        })
                    ));
                let recoverable_length = !length_recovery_attempted
                    && matches!(
                        &generated,
                        Ok(ModelResponse {
                            termination: ResponseTermination::Incomplete(
                                IncompleteReason::MaxOutputTokens
                            ),
                            usage: ion_ai::Usage {
                                output_tokens: Some(output),
                                ..
                            },
                            ..
                        }) if *output < u64::from(self.limits.max_output_tokens)
                    );
                if ((overflow && !emitted_text) || recoverable_length)
                    && !prefix_bound_continuation
                    && !recovered_overflow
                    && self
                        .compact_inner(session, &model, stop, self.keep_bytes(), observe)
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
                break (generated?, tool_catalog);
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
                .is_some_and(|replay| !replay.is_compatible_with(&model.provider))
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
            if truncated_calls {
                assistant_seen_in_turn = true;
                let results = session.record_truncated_assistant(
                    turn,
                    response.message,
                    tool_activities.clone(),
                    response.usage,
                )?;
                for result in results {
                    let activity = calls
                        .iter()
                        .find(|call| call.id == result.call_id)
                        .map_or_else(
                            || ToolActivity::external(&result.name),
                            |call| tool_catalog.activity(call),
                        );
                    observe(AgentEvent::ToolRejected {
                        call_id: result.call_id,
                        name: result.name,
                        activity,
                        output: ToolOutput {
                            value: result.result,
                            images: Vec::new(),
                            is_error: true,
                        },
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
            let complete = if calls.is_empty() {
                if let Some(inbox) = steering {
                    inbox.record_assistant(
                        session,
                        turn,
                        response.message,
                        tool_activities.clone(),
                        response.usage,
                        self.limits,
                    )?
                } else {
                    session.record_assistant_with_activities(
                        turn,
                        response.message,
                        tool_activities.clone(),
                        response.usage,
                        false,
                    )?
                }
            } else {
                session.record_assistant_with_activities(
                    turn,
                    response.message,
                    tool_activities.clone(),
                    response.usage,
                    false,
                )?
            };
            assistant_seen_in_turn = true;
            if complete {
                observe(AgentEvent::Final(final_text.clone()));
                return Ok(final_text);
            }
            for call in calls {
                if stop.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                let activity = tool_catalog.activity(&call);
                observe(AgentEvent::ToolStarted {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    activity: activity.clone(),
                });
                let mut output = if call.raw_arguments.is_some() {
                    ToolOutput {
                        value: serde_json::json!({"error":"tool arguments were not a valid JSON object; submit a corrected call"}),
                        images: Vec::new(),
                        is_error: true,
                    }
                } else {
                    tool_catalog.execute(&call, stop.clone()).await
                };
                if !self.limits.image_input && !output.images.is_empty() {
                    output = ToolOutput {
                        value: serde_json::json!({"error":"selected model route does not support image tool results; choose an image-capable model and read the file again"}),
                        images: Vec::new(),
                        is_error: true,
                    };
                }
                if output.images.iter().any(|image| image.validate().is_err())
                    || serde_json::to_vec(&(&output.value, &output.images))?.len()
                        > self.limits.max_request_bytes
                {
                    output = ToolOutput {
                        value: serde_json::json!({"error":"tool result image data is invalid or exceeds this route's request bound; the tool may already have affected the workspace"}),
                        images: Vec::new(),
                        is_error: true,
                    };
                }
                session.record_tool_result(
                    turn,
                    ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        result: output.value.clone(),
                        images: output.images.clone(),
                        is_error: output.is_error,
                    },
                )?;
                observe(AgentEvent::ToolFinished {
                    call_id: call.id,
                    name: call.name,
                    activity,
                    output,
                });
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TurnAccepted {
        turn: u64,
    },
    TextDelta(String),
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
    ToolFinished {
        call_id: String,
        name: String,
        activity: ToolActivity,
        output: ToolOutput,
    },
    ToolRejected {
        call_id: String,
        name: String,
        activity: ToolActivity,
        output: ToolOutput,
    },
    InterruptedCalls(usize),
    Final(String),
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("prompt is empty")]
    EmptyPrompt,
    #[error("steering input is not a valid user message")]
    InvalidUserInput,
    #[error(
        "selected model route does not declare image input; choose an image-capable model or configure the custom route with --images"
    )]
    ImagesUnsupported,
    #[error("turn was cancelled")]
    Cancelled,
    #[error(
        "model input exceeds the context or request-size limit after available compaction; shorten the current input or, for a context-window limit, select a larger-context model"
    )]
    ContextTooLarge,
    #[error("model returned an invalid context summary")]
    InvalidSummary,
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

impl AgentError {
    fn end_reason(&self) -> TurnEndReason {
        match self {
            Self::Cancelled => TurnEndReason::Cancelled,
            Self::Provider(error) => TurnEndReason::Failed(format!("provider: {:?}", error.kind)),
            Self::ContextTooLarge => TurnEndReason::Failed("context_too_large".into()),
            Self::InvalidSummary => TurnEndReason::Failed("invalid_summary".into()),
            Self::IncompleteModelResponse => {
                TurnEndReason::Failed("incomplete_model_response".into())
            }
            Self::InvalidProviderReplay => TurnEndReason::Failed("invalid_provider_replay".into()),
            Self::ReplayContextChanged => TurnEndReason::Failed("replay_context_changed".into()),
            Self::InvalidToolCall => TurnEndReason::Failed("invalid_tool_call".into()),
            Self::EmptyPrompt
            | Self::InvalidUserInput
            | Self::ImagesUnsupported
            | Self::Session(_)
            | Self::Json(_) => TurnEndReason::Failed("agent_error".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CodingSession, LocalTools, ToolDefinition};
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

    #[test]
    fn coding_output_budget_uses_model_ceiling_and_remaining_context() {
        let root = std::env::temp_dir().join(format!("ion-budget-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let agent = Agent::new(
            Arc::new(ScriptedModelService::new([])),
            Arc::new(LocalTools::new(&root).unwrap()),
        )
        .with_limits(AgentLimits {
            max_output_tokens: 128_000,
            context_window_tokens: Some(200_000),
            ..AgentLimits::default()
        });
        assert_eq!(agent.output_budget(900, 300, 128_000), Some(128_000));
        assert_eq!(agent.output_budget(300_000, 100_000, 128_000), Some(91_808));
        assert_eq!(agent.output_budget(600_000, 200_000, 128_000), None);
        assert_eq!(agent.output_budget(9 * 1024 * 1024, 300, 128_000), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn coding_requests_send_the_context_clamped_model_ceiling() {
        let root = std::env::temp_dir().join(format!("ion-budget-run-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("first".into())]),
            response(vec![Content::Text("second".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()))
            .with_limits(AgentLimits {
                max_output_tokens: 128_000,
                context_window_tokens: Some(200_000),
                ..AgentLimits::default()
            });
        agent
            .submit(
                &session,
                model(),
                "short".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        agent
            .submit(
                &session,
                model(),
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
        let agent = Agent::new(service.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        let request = ModelRequest {
            model: model(),
            instructions: None,
            messages: Vec::new(),
            tools: Vec::new(),
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
        generate_with_retry(
            &agent.model,
            request.clone(),
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
        assert_eq!(service.requests(), vec![request.clone(), request.clone()]);
        assert_eq!(service.requests().len(), 2);

        let service = Arc::new(ScriptedModelService::new([
            Script::Stream(vec![ModelStreamEvent::TextDelta("partial".into())]),
            response(vec![Content::Text("should not be used".into())]),
        ]));
        let agent = Agent::new(service.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        let mut visible = String::new();
        let error = generate_with_retry(
            &agent.model,
            request.clone(),
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
        let agent = Agent::new(service.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        let error = generate_with_retry(
            &agent.model,
            request,
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
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
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
        let agent = Agent::new(service.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        let mut rebases = 0;
        assert_eq!(
            agent
                .submit(
                    &session,
                    model(),
                    "second".into(),
                    "instructions".into(),
                    CancellationToken::new(),
                    |event| {
                        if matches!(event, AgentEvent::ProviderReplayRebased) {
                            rebases += 1;
                        }
                    },
                )
                .await
                .unwrap(),
            "done"
        );
        assert_eq!(rebases, 1);
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
    async fn signed_tool_continuation_does_not_rebase_or_repeat_tool() {
        let root = std::env::temp_dir().join(format!("ion-tool-prefix-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file.txt"), "content").unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
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
        let agent = Agent::new(service.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        let error = agent
            .submit(
                &session,
                model(),
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
        let session = CodingSession::create(&path, &root).unwrap();
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
        let tools = Arc::new(LocalTools::new(&root).unwrap());
        let agent = Agent::new(scripts, tools.clone());
        let answer = agent
            .submit(
                &session,
                model(),
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
        let reopened = CodingSession::open(&path).unwrap();
        let scripts = Arc::new(ScriptedModelService::new([response(vec![Content::Text(
            "still here".into(),
        )])]));
        let agent = Agent::new(scripts, tools);
        let answer = agent
            .submit(
                &reopened,
                model(),
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
    async fn tool_images_are_committed_replayed_and_rejected_on_a_text_only_route() {
        struct ImageTool;
        impl ToolHost for ImageTool {
            fn definitions(&self) -> Vec<ToolDefinition> {
                vec![ToolDefinition::external(ToolSpec {
                    name: "picture".into(),
                    description: "picture".into(),
                    input_schema: serde_json::json!({"type":"object"}),
                })]
            }
            fn execute<'a>(
                &'a self,
                _: &'a ToolCall,
                _: CancellationToken,
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
        let session = CodingSession::create(&path, &root).unwrap();
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
        let agent = Agent::new(scripts.clone(), Arc::new(ImageTool)).with_limits(AgentLimits {
            image_input: true,
            ..AgentLimits::default()
        });
        agent
            .submit(
                &session,
                model(),
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
        let reopened = CodingSession::open(&path).unwrap();
        assert!(
            matches!(&reopened.messages().unwrap()[2].content[0], Content::ToolResult(result) if result.images.len() == 1)
        );
        let text_service = Arc::new(ScriptedModelService::new([]));
        let text_agent = Agent::new(text_service.clone(), Arc::new(ImageTool));
        let error = text_agent
            .submit(
                &reopened,
                model(),
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
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
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
        let agent = Agent::new(service.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        let answer = agent
            .submit(
                &session,
                model(),
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
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
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
        let agent = Agent::new(service.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        let stop = CancellationToken::new();
        let mut trigger = Some(stop.clone());
        let result = agent
            .submit(
                &session,
                model(),
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
        let session = CodingSession::create(&path, &root).unwrap();
        let service = Arc::new(ScriptedModelService::new([
            response(Vec::new()),
            response(vec![Content::Text(" \n".into())]),
            response(vec![Content::Text("working again".into())]),
        ]));
        let agent = Agent::new(service, Arc::new(LocalTools::new(&root).unwrap()));
        for prompt in ["empty", "blank"] {
            let error = agent
                .submit(
                    &session,
                    model(),
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
        let reopened = CodingSession::open(&path).unwrap();
        assert_eq!(
            agent
                .submit(
                    &reopened,
                    model(),
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
        let session = CodingSession::create(&path, &root).unwrap();
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
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        let mut rejected = Vec::new();
        assert_eq!(
            agent
                .submit(
                    &session,
                    model(),
                    "write the file".into(),
                    "test".into(),
                    CancellationToken::new(),
                    |event| {
                        if let AgentEvent::ToolRejected { call_id, .. } = event {
                            rejected.push(call_id);
                        }
                    },
                )
                .await
                .unwrap(),
            "done"
        );
        assert_eq!(rejected, ["partial", "partial-2"]);
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
        let reopened = CodingSession::open(&path).unwrap();
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
        let session = CodingSession::create(&path, &root).unwrap();
        let scripts = Arc::new(ScriptedModelService::new([response(vec![Content::Text(
            "done".into(),
        )])]));
        let agent = Agent::new(scripts, Arc::new(LocalTools::new(&root).unwrap())).with_limits(
            AgentLimits {
                max_request_bytes: 80 * 1024 * 1024,
                ..AgentLimits::default()
            },
        );
        let steering = SteeringInbox::default();
        let prompt = "\0".repeat(11 * 1024 * 1024);
        steering.push(prompt.clone());
        let result = agent
            .submit_with_steering(
                &session,
                model(),
                "start".into(),
                "test".into(),
                CancellationToken::new(),
                &steering,
                |_| {},
            )
            .await;
        assert!(matches!(result, Err(AgentError::Session(_))));
        assert_eq!(steering.take_uncommitted(), vec![user_text(prompt)]);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn assistant_and_steering_commit_together_or_remain_unpublished() {
        let root =
            std::env::temp_dir().join(format!("ion-steering-batch-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = CodingSession::create(&path, &root).unwrap();
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
        let agent = Agent::new(scripts, Arc::new(LocalTools::new(&root).unwrap())).with_limits(
            AgentLimits {
                max_request_bytes: 80 * 1024 * 1024,
                ..AgentLimits::default()
            },
        );
        let steering = SteeringInbox::default();
        let prompt = "\0".repeat(11 * 1024 * 1024);
        let result = agent
            .submit_with_steering(
                &session,
                model(),
                "start".into(),
                "test".into(),
                CancellationToken::new(),
                &steering,
                |event| {
                    if matches!(event, AgentEvent::TextDelta(_)) {
                        steering.push(prompt.clone());
                    }
                },
            )
            .await;
        assert!(matches!(result, Err(AgentError::Session(_))));
        assert_eq!(steering.take_uncommitted(), vec![user_text(prompt)]);
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
        let session = CodingSession::create(&path, &root).unwrap();
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
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        let steering = SteeringInbox::default();
        let answer = agent
            .submit_with_steering(
                &session,
                model(),
                "start".into(),
                "test".into(),
                CancellationToken::new(),
                &steering,
                |event| {
                    if matches!(event, AgentEvent::TextDelta(_)) {
                        steering.push("also answer this".into());
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
    async fn image_steering_is_durable_and_replayed_in_the_next_model_step() {
        let root =
            std::env::temp_dir().join(format!("ion-image-steering-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = CodingSession::create(&path, &root).unwrap();
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
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()))
            .with_limits(AgentLimits {
                image_input: true,
                ..AgentLimits::default()
            });
        let steering = SteeringInbox::default();
        let answer = agent
            .submit_with_steering(
                &session,
                model(),
                "start".into(),
                "test".into(),
                CancellationToken::new(),
                &steering,
                |event| {
                    if matches!(event, AgentEvent::TextDelta(_)) {
                        steering
                            .push_message(Message {
                                role: Role::User,
                                content: vec![
                                    Content::Text("look at this".into()),
                                    Content::Image(image.clone()),
                                ],
                                provider_replay: None,
                            })
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
        let reopened = CodingSession::open(&path).unwrap();
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
    fn image_steering_cannot_commit_on_a_text_only_route() {
        let root =
            std::env::temp_dir().join(format!("ion-image-steering-route-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
        let (turn, _) = session
            .begin_turn_message(user_text("start".into()), model())
            .unwrap();
        let steering = SteeringInbox::default();
        let input = Message {
            role: Role::User,
            content: vec![Content::Text("look".into()), Content::Image(tiny_image())],
            provider_replay: None,
        };
        steering.push_message(input.clone()).unwrap();
        assert!(matches!(
            steering.record_pending(&session, turn, AgentLimits::default()),
            Err(AgentError::ImagesUnsupported)
        ));
        assert_eq!(steering.take_uncommitted(), vec![input]);
        assert_eq!(session.view().unwrap().entries.len(), 1);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn steering_is_recorded_before_the_next_model_step_in_one_turn() {
        let root = std::env::temp_dir().join(format!("ion-steering-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = CodingSession::create(&path, &root).unwrap();
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::ToolCall(ToolCall {
                id: "call-1".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path":"file.txt"}),
                raw_arguments: None,
            })]),
            response(vec![Content::Text("done".into())]),
        ]));
        std::fs::write(root.join("file.txt"), "content").unwrap();
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        let steering = Arc::new(SteeringInbox::default());
        let sender = steering.clone();
        agent
            .submit_with_steering(
                &session,
                model(),
                "read file".into(),
                "test".into(),
                CancellationToken::new(),
                &steering,
                move |event| {
                    if matches!(event, AgentEvent::ToolFinished { .. }) {
                        sender.push("also check the content".into());
                    }
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
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn manual_compaction_uses_no_tools_and_keeps_raw_history() {
        let root = std::env::temp_dir().join(format!("ion-compact-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = CodingSession::create(&path, &root).unwrap();
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("first result".into())]),
            response(vec![Content::Text("First task is complete.".into())]),
            response(vec![Content::Text("continued".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        agent
            .submit(
                &session,
                model(),
                "first task".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert!(
            agent
                .compact(&session, model(), CancellationToken::new(), |_| {})
                .await
                .unwrap()
        );
        assert!(scripts.requests()[1].tools.is_empty());
        assert_eq!(session.messages().unwrap().len(), 2);
        assert_eq!(session.context_messages().unwrap().len(), 1);
        agent
            .submit(
                &session,
                model(),
                "continue".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(scripts.requests()[2].messages.len(), 2);
        assert_eq!(session.messages().unwrap().len(), 4);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn provider_overflow_compacts_and_retries_only_the_model_request() {
        let root = std::env::temp_dir().join(format!("ion-overflow-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
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
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        for prompt in ["first task", "next task"] {
            agent
                .submit(
                    &session,
                    model(),
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
    async fn mimo_zero_output_at_window_compacts_before_retry() {
        let root = std::env::temp_dir().join(format!("ion-mimo-overflow-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("earlier result".into())]),
            Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: Vec::new(),
                    provider_replay: None,
                },
                usage: Usage::known(49_900, 0),
                termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
                returned_model: Some("mimo-v2.6-flash".into()),
            })]),
            response(vec![Content::Text("Earlier result is complete.".into())]),
            response(vec![Content::Text("continued".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()))
            .with_limits(AgentLimits {
                context_window_tokens: Some(50_000),
                ..AgentLimits::default()
            });
        let mimo = ModelRef {
            provider: "xiaomi".into(),
            model: "mimo-v2.6-flash".into(),
        };
        for prompt in ["first", "second"] {
            agent
                .submit(
                    &session,
                    mimo.clone(),
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
    async fn short_length_stop_compacts_and_retries_without_dispatching_partial_call() {
        let root = std::env::temp_dir().join(format!("ion-short-length-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
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
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        agent
            .submit(
                &session,
                model(),
                "first".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let mut restarted = false;
        let answer = agent
            .submit(
                &session,
                model(),
                "second".into(),
                "test".into(),
                CancellationToken::new(),
                |event| {
                    if matches!(event, AgentEvent::ResponseRestarted) {
                        restarted = true;
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(answer, "complete answer");
        assert!(restarted);
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
    async fn full_output_limit_does_not_trigger_context_recovery() {
        let root = std::env::temp_dir().join(format!("ion-full-length-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
        let limit = 128;
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("earlier work".into())]),
            Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("unfinished".into())],
                    provider_replay: None,
                },
                usage: Usage::known(100, limit),
                termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
                returned_model: None,
            })]),
            response(vec![Content::Text("unexpected retry".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()))
            .with_limits(AgentLimits {
                max_output_tokens: limit as u32,
                ..AgentLimits::default()
            });
        agent
            .submit(
                &session,
                model(),
                "first".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        let error = agent
            .submit(
                &session,
                model(),
                "second".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::IncompleteModelResponse));
        assert_eq!(scripts.requests().len(), 2);
        assert!(session.view().unwrap().compacted_through.is_none());
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn invented_tool_returns_error_and_model_can_correct_it() {
        let root = std::env::temp_dir().join(format!("ion-invented-tool-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::ToolCall(ToolCall {
                id: "invented".into(),
                name: "not_a_tool".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
            })]),
            response(vec![Content::Text("I can use read instead.".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        let answer = agent
            .submit(
                &session,
                model(),
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
            matches!(result, Content::ToolResult(result) if result.result["error"] == "unknown tool: not_a_tool")
        );
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn malformed_tool_arguments_return_error_without_dispatch() {
        struct NeverDispatch;
        impl ToolHost for NeverDispatch {
            fn definitions(&self) -> Vec<ToolDefinition> {
                Vec::new()
            }
            fn execute<'a>(
                &'a self,
                _call: &'a ToolCall,
                _stop: CancellationToken,
            ) -> BoxFuture<'a, ToolOutput> {
                Box::pin(async { panic!("malformed call was dispatched") })
            }
        }
        let root =
            std::env::temp_dir().join(format!("ion-malformed-call-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::ToolCall(ToolCall {
                id: "broken".into(),
                name: "exec".into(),
                arguments: serde_json::json!({}),
                raw_arguments: Some("{\"command\":\"touch should-not-exist\"".into()),
            })]),
            response(vec![Content::Text("I need to correct the call".into())]),
        ]));
        let agent = Agent::new(scripts, Arc::new(NeverDispatch));
        agent
            .submit(
                &session,
                model(),
                "try a command".into(),
                "test".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert!(!root.join("should-not-exist").exists());
        assert!(matches!(
            &session.messages().unwrap()[2].content[0],
            Content::ToolResult(ToolResult { is_error: true, .. })
        ));
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_summary_does_not_change_the_context_projection() {
        let root = std::env::temp_dir().join(format!("ion-summary-fail-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
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
        let agent = Agent::new(scripts, Arc::new(LocalTools::new(&root).unwrap()));
        assert!(
            agent
                .compact(&session, model(), CancellationToken::new(), |_| {})
                .await
                .is_err()
        );
        assert_eq!(session.context_messages().unwrap(), before);
        assert_eq!(session.view().unwrap().compacted_through, None);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
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
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
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
            Arc::new(LocalTools::new(&root).unwrap()),
        );
        assert!(matches!(
            agent.compact(&session, model(), stop, |_| {}).await,
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
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
        let scripts = Arc::new(ScriptedModelService::new([
            response(vec![Content::Text("x".repeat(1_200))]),
            response(vec![Content::Text("First task completed.".into())]),
            response(vec![Content::Text("continued".into())]),
        ]));
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()))
            .with_limits(AgentLimits {
                // Leave room for the built-in tool schemas; the second Turn
                // still has to compact the first Turn's long answer.
                max_request_bytes: 3_200,
                ..AgentLimits::default()
            });
        for prompt in ["first", "second"] {
            agent
                .submit(
                    &session,
                    model(),
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
    async fn long_saved_history_compacts_in_bounded_steps() {
        let root = std::env::temp_dir().join(format!("ion-long-context-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = CodingSession::create(&path, &root).unwrap();
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
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()))
            .with_limits(AgentLimits {
                max_request_bytes: 2_000,
                ..AgentLimits::default()
            });
        let mut cuts = Vec::new();
        assert!(
            agent
                .compact(&session, model(), CancellationToken::new(), |event| {
                    if let AgentEvent::ContextCompacted { through_entry } = event {
                        cuts.push(through_entry);
                    }
                })
                .await
                .unwrap()
        );
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
        let reopened = CodingSession::open(&path).unwrap();
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
    async fn oversized_atomic_turn_reports_capacity_without_changing_history() {
        let root =
            std::env::temp_dir().join(format!("ion-atomic-context-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
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
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()))
            .with_limits(AgentLimits {
                max_request_bytes: 2_000,
                ..AgentLimits::default()
            });
        assert!(matches!(
            agent
                .compact(&session, model(), CancellationToken::new(), |_| {})
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
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
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
        let agent = Agent::new(scripts.clone(), Arc::new(LocalTools::new(&root).unwrap()));
        agent
            .submit(
                &session,
                model(),
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
                    &session,
                    model(),
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
        let session = CodingSession::create(&path, &root).unwrap();
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
        let reopened = CodingSession::open(&path).unwrap();
        let scripts = Arc::new(ScriptedModelService::new([response(vec![Content::Text(
            "inspected".into(),
        )])]));
        let agent = Agent::new(scripts, Arc::new(LocalTools::new(&root).unwrap()));
        let mut interrupted = 0;
        agent
            .submit(
                &reopened,
                model(),
                "inspect first".into(),
                "test".into(),
                CancellationToken::new(),
                |event| {
                    if let AgentEvent::InterruptedCalls(count) = event {
                        interrupted = count;
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(interrupted, 1);
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
