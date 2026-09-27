//! One coding loop for library, headless and terminal clients.
use std::sync::Arc;

use futures_util::StreamExt;
use ion_ai::{
    BoxFuture, Content, GenerationControls, ModelRef, ModelRequest, ModelService, ProviderError,
    Reasoning, ResponseTermination, Role, ToolCall, ToolChoice, ToolResult, ToolSpec,
};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::session::{Session, SessionError, TurnEndReason};

#[derive(Debug, Clone, Copy)]
pub struct AgentLimits {
    pub max_steps: usize,
    pub max_request_bytes: usize,
    pub max_output_tokens: u32,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_steps: 80,
            max_request_bytes: 2 * 1024 * 1024,
            max_output_tokens: 16_384,
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
            .drive(session, turn, model, instructions, &stop, &mut observe)
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

    async fn drive<F>(
        &self,
        session: &Session,
        turn: u64,
        model: ModelRef,
        instructions: String,
        stop: &CancellationToken,
        observe: &mut F,
    ) -> Result<String, AgentError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        for _ in 0..self.limits.max_steps {
            if stop.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let request = ModelRequest {
                model: model.clone(),
                instructions: Some(instructions.clone()),
                messages: session.messages()?,
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
            if serde_json::to_vec(&request)?.len() > self.limits.max_request_bytes {
                return Err(AgentError::ContextTooLarge);
            }
            let stream = tokio::select! {
                result = self.model.stream(request) => result?,
                () = stop.cancelled() => return Err(AgentError::Cancelled),
            };
            tokio::pin!(stream);
            let response = loop {
                let event = tokio::select! {
                    item = stream.next() => item,
                    () = stop.cancelled() => return Err(AgentError::Cancelled),
                };
                match event {
                    Some(Ok(ion_ai::ModelStreamEvent::TextDelta(text))) => {
                        observe(AgentEvent::TextDelta(text))
                    }
                    Some(Ok(ion_ai::ModelStreamEvent::ToolCall(_)))
                    | Some(Ok(ion_ai::ModelStreamEvent::Usage(_))) => {}
                    Some(Ok(ion_ai::ModelStreamEvent::Completed(response))) => break response,
                    Some(Err(error)) => return Err(AgentError::Provider(error)),
                    None => return Err(AgentError::IncompleteModelResponse),
                }
            };
            if response.message.role != Role::Assistant
                || !matches!(response.termination, ResponseTermination::Completed)
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
            let complete = session.record_assistant(turn, response.message)?;
            if complete {
                observe(AgentEvent::Final(final_text.clone()));
                return Ok(final_text);
            }
            for call in calls {
                if stop.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                observe(AgentEvent::ToolStarted {
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });
                let output = self.tools.execute(&call, stop.clone()).await;
                session.record_tool_result(
                    turn,
                    ToolResult {
                        call_id: call.id,
                        name: call.name.clone(),
                        result: output.value.clone(),
                    },
                )?;
                observe(AgentEvent::ToolFinished {
                    name: call.name,
                    output,
                });
            }
        }
        Err(AgentError::StepLimit)
    }
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ToolStarted { name: String, arguments: Value },
    ToolFinished { name: String, output: ToolOutput },
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
    #[error("model context is too large; start a new session or reduce history")]
    ContextTooLarge,
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
                }),
                Content::ToolCall(ToolCall {
                    id: "call-2".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path":"answer.txt"}),
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
                    })],
                    provider_replay: None,
                },
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
