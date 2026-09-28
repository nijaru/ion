//! One coding loop for library, headless and terminal clients.
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use ion_ai::{
    BoxFuture, Content, GenerationControls, IncompleteReason, ModelRef, ModelRequest,
    ModelResponse, ModelService, ModelStreamEvent, ProviderError, ProviderErrorKind, Reasoning,
    ResponseTermination, Role, ToolCall, ToolChoice, ToolResult, ToolSpec,
};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::session::{Session, SessionError, TurnEndReason};

#[derive(Debug, Clone, Copy)]
pub struct AgentLimits {
    pub max_steps: usize,
    pub max_request_bytes: usize,
    pub max_output_tokens: u32,
    pub context_window_tokens: Option<u32>,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_steps: 80,
            max_request_bytes: 8 * 1024 * 1024,
            max_output_tokens: 16_384,
            context_window_tokens: None,
        }
    }
}

pub struct Agent {
    model: Arc<dyn ModelService>,
    tools: Arc<dyn ToolHost>,
    limits: AgentLimits,
}

impl Agent {
    pub fn new(model: Arc<dyn ModelService>, tools: Arc<dyn ToolHost>) -> Self {
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

    fn request_fits(&self, bytes: usize, output_tokens: u32) -> bool {
        if bytes > self.limits.max_request_bytes {
            return false;
        }
        self.limits.context_window_tokens.is_none_or(|window| {
            let estimated_input = bytes.div_ceil(3) as u64;
            estimated_input + u64::from(output_tokens) + 8_192 <= u64::from(window)
        })
    }

    fn keep_bytes(&self) -> usize {
        let model_budget = self
            .limits
            .context_window_tokens
            .map_or(usize::MAX, |window| {
                (window.saturating_sub(self.limits.max_output_tokens + 8_192) as usize)
                    .saturating_mul(3)
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
            if self.request_fits(serde_json::to_vec(&request)?.len(), output_tokens) {
                break (plan.through_entry, plan.chunked, request);
            }
            budget /= 2;
            if budget == 0 {
                return Err(AgentError::ContextTooLarge);
            }
        };
        let response = self.generate(request, stop, &mut |_| {}).await?;
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
        session.record_compaction(through_entry, summary, response.usage)?;
        observe(AgentEvent::ContextCompacted { through_entry });
        Ok(Some(chunked))
    }

    async fn generate<F>(
        &self,
        request: ModelRequest,
        stop: &CancellationToken,
        observe: &mut F,
    ) -> Result<ModelResponse, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        for attempt in 0..=2 {
            let (result, observed) = self.generate_once(request.clone(), stop, observe).await;
            match result {
                Err(AgentError::Provider(error))
                    if !observed && attempt < 2 && retryable_provider_error(&error) =>
                {
                    // A server's pacing takes precedence over local backoff.
                    // Leave long waits to the caller instead of holding a Turn
                    // open for an unbounded provider-requested interval.
                    let delay_ms = error.retry_after_ms.unwrap_or(500 * (1 << attempt));
                    if delay_ms > 60_000 {
                        return Err(AgentError::Provider(error));
                    }
                    let delay = Duration::from_millis(delay_ms);
                    observe(AgentEvent::ProviderRetry {
                        attempt: attempt + 1,
                        max_retries: 2,
                        delay_ms: delay.as_millis() as u64,
                    });
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {},
                        () = stop.cancelled() => return Err(AgentError::Cancelled),
                    }
                }
                other => return other,
            }
        }
        unreachable!("bounded retry loop returns on its final attempt")
    }

    async fn generate_once<F>(
        &self,
        request: ModelRequest,
        stop: &CancellationToken,
        observe: &mut F,
    ) -> (Result<ModelResponse, AgentError>, bool)
    where
        F: FnMut(AgentEvent) + Send,
    {
        let stream = tokio::select! {
            result = self.model.stream(request) => match result {
                Ok(stream) => stream,
                Err(error) => return (Err(AgentError::Provider(error)), false),
            },
            () = stop.cancelled() => return (Err(AgentError::Cancelled), false),
        };
        tokio::pin!(stream);
        let mut observed = false;
        loop {
            let event = tokio::select! {
                item = stream.next() => item,
                () = stop.cancelled() => return (Err(AgentError::Cancelled), observed),
            };
            match event {
                Some(Ok(ModelStreamEvent::TextDelta(text))) => {
                    observed = true;
                    observe(AgentEvent::TextDelta(text));
                }
                Some(Ok(ModelStreamEvent::ToolCall(_))) | Some(Ok(ModelStreamEvent::Usage(_))) => {
                    observed = true;
                }
                Some(Ok(ModelStreamEvent::Completed(response))) => return (Ok(response), observed),
                Some(Err(error)) => return (Err(AgentError::Provider(error)), observed),
                None => return (Err(AgentError::IncompleteModelResponse), observed),
            }
        }
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
        self.submit_inner(
            session,
            model,
            prompt,
            instructions,
            stop,
            None,
            &mut observe,
        )
        .await
    }

    /// Accept user steering at model-step boundaries during an active turn.
    /// Messages still in the receiver when the turn ends belong to the host.
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
        steering: &mut mpsc::UnboundedReceiver<String>,
        mut observe: F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        self.submit_inner(
            session,
            model,
            prompt,
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
        prompt: String,
        instructions: String,
        stop: CancellationToken,
        steering: Option<&mut mpsc::UnboundedReceiver<String>>,
        observe: &mut F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        if prompt.trim().is_empty() {
            return Err(AgentError::EmptyPrompt);
        }
        let _guard = tokio::select! {
            guard = session.submit_gate.lock() => guard,
            () = stop.cancelled() => return Err(AgentError::Cancelled),
        };
        if stop.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let (turn, interrupted) = session.begin_turn(prompt, model.clone())?;
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
        mut steering: Option<&mut mpsc::UnboundedReceiver<String>>,
        observe: &mut F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        for _ in 0..self.limits.max_steps {
            if stop.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            for prompt in drain_steering(&mut steering) {
                session.record_steering(turn, prompt)?;
            }
            let mut recovered_overflow = false;
            let response = loop {
                let request = ModelRequest {
                    model: model.clone(),
                    instructions: Some(instructions.clone()),
                    messages: session.context_messages()?,
                    tools: self.tools.specs(),
                    controls: GenerationControls {
                        max_output_tokens: self.limits.max_output_tokens,
                        temperature: None,
                        top_p: None,
                        reasoning: Reasoning::ProviderDefault,
                        tool_choice: ToolChoice::Auto,
                        parallel_tool_calls: true,
                    },
                };
                if !self.request_fits(
                    serde_json::to_vec(&request)?.len(),
                    self.limits.max_output_tokens,
                ) {
                    if self
                        .compact_inner(session, &model, stop, self.keep_bytes(), observe)
                        .await?
                        .is_some()
                    {
                        continue;
                    }
                    return Err(AgentError::ContextTooLarge);
                }
                let mut emitted_text = false;
                let generated = self
                    .generate(request, stop, &mut |event| {
                        if matches!(event, AgentEvent::TextDelta(_)) {
                            emitted_text = true;
                        }
                        observe(event);
                    })
                    .await;
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
                if overflow
                    && !emitted_text
                    && !recovered_overflow
                    && self
                        .compact_inner(session, &model, stop, self.keep_bytes(), observe)
                        .await?
                        .is_some()
                {
                    recovered_overflow = true;
                    continue;
                }
                break generated?;
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
            if truncated_calls {
                let results =
                    session.record_truncated_assistant(turn, response.message, response.usage)?;
                for result in results {
                    observe(AgentEvent::ToolRejected {
                        call_id: result.call_id,
                        name: result.name,
                        output: ToolOutput {
                            value: result.result,
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
            // Keep steering in the channel until tools settle. Cancellation
            // during a tool must return unsent input to the host.
            let pending_steering = if calls.is_empty() {
                drain_steering(&mut steering)
            } else {
                Vec::new()
            };
            let complete = session.record_assistant(
                turn,
                response.message,
                response.usage,
                !pending_steering.is_empty(),
            )?;
            if complete {
                observe(AgentEvent::Final(final_text.clone()));
                return Ok(final_text);
            }
            for call in calls {
                if stop.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                observe(AgentEvent::ToolStarted {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
                let output = if call.raw_arguments.is_some() {
                    ToolOutput {
                        value: serde_json::json!({"error":"tool arguments were not a valid JSON object; submit a corrected call"}),
                        is_error: true,
                    }
                } else {
                    self.tools.execute(&call, stop.clone()).await
                };
                session.record_tool_result(
                    turn,
                    ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        result: output.value.clone(),
                        is_error: output.is_error,
                    },
                )?;
                observe(AgentEvent::ToolFinished {
                    call_id: call.id,
                    name: call.name,
                    output,
                });
            }
            for prompt in pending_steering {
                session.record_steering(turn, prompt)?;
            }
        }
        Err(AgentError::StepLimit)
    }
}

fn retryable_provider_error(error: &ProviderError) -> bool {
    matches!(
        error.kind,
        ProviderErrorKind::Transport
            | ProviderErrorKind::Timeout
            | ProviderErrorKind::RateLimited
            | ProviderErrorKind::Overloaded
            | ProviderErrorKind::Server
    )
}

fn drain_steering(receiver: &mut Option<&mut mpsc::UnboundedReceiver<String>>) -> Vec<String> {
    let mut prompts = Vec::new();
    if let Some(receiver) = receiver {
        while let Ok(prompt) = receiver.try_recv() {
            if !prompt.trim().is_empty() {
                prompts.push(prompt);
            }
        }
    }
    prompts
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ProviderRetry {
        attempt: usize,
        max_retries: usize,
        delay_ms: u64,
    },
    ContextCompacted {
        through_entry: u64,
    },
    ToolStarted {
        call_id: String,
        name: String,
        arguments: Value,
    },
    ToolFinished {
        call_id: String,
        name: String,
        output: ToolOutput,
    },
    ToolRejected {
        call_id: String,
        name: String,
        output: ToolOutput,
    },
    InterruptedCalls(usize),
    Final(String),
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub value: Value,
    pub is_error: bool,
}

pub trait ToolHost: Send + Sync {
    fn specs(&self) -> Vec<ToolSpec>;
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolOutput>;
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("prompt is empty")]
    EmptyPrompt,
    #[error("turn was cancelled")]
    Cancelled,
    #[error(
        "model context is too large after available compaction; shorten the current input or select a larger-context model"
    )]
    ContextTooLarge,
    #[error("model returned an invalid context summary")]
    InvalidSummary,
    #[error("model stream ended without a complete assistant response")]
    IncompleteModelResponse,
    #[error("model returned continuation material for another provider")]
    InvalidProviderReplay,
    #[error("model returned an invalid or duplicate tool call")]
    InvalidToolCall,
    #[error("turn exceeded its model-step limit")]
    StepLimit,
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
            Self::StepLimit => TurnEndReason::StepLimit,
            Self::Provider(error) => TurnEndReason::Failed(format!("provider: {:?}", error.kind)),
            Self::ContextTooLarge => TurnEndReason::Failed("context_too_large".into()),
            Self::InvalidSummary => TurnEndReason::Failed("invalid_summary".into()),
            Self::IncompleteModelResponse => {
                TurnEndReason::Failed("incomplete_model_response".into())
            }
            Self::InvalidProviderReplay => TurnEndReason::Failed("invalid_provider_replay".into()),
            Self::InvalidToolCall => TurnEndReason::Failed("invalid_tool_call".into()),
            Self::EmptyPrompt | Self::Session(_) | Self::Json(_) => {
                TurnEndReason::Failed("agent_error".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CodingSession, LocalTools};
    use ion_ai::{Message, ModelResponse, ModelStreamEvent, Script, ScriptedModelService, Usage};

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
        agent
            .generate(request.clone(), &CancellationToken::new(), &mut |event| {
                if let AgentEvent::ProviderRetry { delay_ms, .. } = event {
                    retry_delay = Some(delay_ms);
                }
            })
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
        let error = agent
            .generate(request.clone(), &CancellationToken::new(), &mut |event| {
                if let AgentEvent::TextDelta(text) = event {
                    visible.push_str(&text);
                }
            })
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
        let error = agent
            .generate(request, &CancellationToken::new(), &mut |_| {})
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::Provider(_)));
        assert_eq!(service.requests().len(), 1);
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
        assert!(matches!(
            &session.view().unwrap().entries[1],
            crate::session::SessionEntry::Assistant {
                termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
                ..
            }
        ));
        drop(session);
        let reopened = CodingSession::open(&path).unwrap();
        assert!(reopened.view().unwrap().unfinished_turn.is_none());
        drop(reopened);
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
        let (sender, mut steering) = mpsc::unbounded_channel();
        agent
            .submit_with_steering(
                &session,
                model(),
                "read file".into(),
                "test".into(),
                CancellationToken::new(),
                &mut steering,
                move |event| {
                    if matches!(event, AgentEvent::ToolFinished { .. }) {
                        sender.send("also check the content".into()).unwrap();
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
            fn specs(&self) -> Vec<ToolSpec> {
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
                max_request_bytes: 2_500,
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
