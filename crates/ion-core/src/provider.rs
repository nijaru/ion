//! Provider port used by the turn engine.

use std::future::Future;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::ids::TurnId;
use crate::tool::{ToolCall, ToolResult, ToolSpec};

/// What a provider is asked to do for one turn: the turn to drive, the
/// user prompt, and the tool specs the controller can dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequest {
    pub turn_id: TurnId,
    pub prompt: String,
    pub tools: Vec<ToolSpec>,
}

/// Signals flowing provider → controller. The controller owns the turn,
/// so every variant carries `turn_id` to disambiguate concurrent/stale
/// signals from a still-running provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSignal {
    TextDelta {
        turn_id: TurnId,
        text: String,
    },
    /// The provider wants the controller to run a tool and feed the
    /// result back through the dedicated tool channel.
    ToolCallRequest {
        turn_id: TurnId,
        call: ToolCall,
    },
    Completed {
        turn_id: TurnId,
    },
    Failed {
        turn_id: TurnId,
        message: String,
    },
    Cancelled {
        turn_id: TurnId,
    },
    /// The provider's task finished without emitting a terminal signal.
    ProviderExited {
        turn_id: TurnId,
    },
}

/// A provider adapter that drives a deterministic scripted transcript.
///
/// Each [`ScriptedMessage`] is either text (optionally delayed, to exercise
/// cancellation mid-flush) or a tool call the controller executes before
/// handing the result back on the tool channel. The provider then resumes
/// the script, simulating a model reacting to tool output.
pub trait Provider: Send + Sync + 'static {
    fn run(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
        incoming: mpsc::Receiver<ToolResult>,
    ) -> impl Future<Output = ()> + Send;
}

/// A step in a scripted transcript.
#[derive(Debug, Clone)]
pub enum ScriptedMessage {
    /// Emit `text` as an assistant text delta. `delay` is waited first
    /// (and is cancellation-aware).
    Text { delay: Duration, text: String },
    /// Request a tool call and wait for its result before continuing.
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
}

impl ScriptedMessage {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            delay: Duration::ZERO,
            text: text.into(),
        }
    }

    #[must_use]
    pub fn delayed(delay: Duration, text: impl Into<String>) -> Self {
        Self::Text {
            delay,
            text: text.into(),
        }
    }

    #[must_use]
    pub fn tool(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self::ToolCall {
            name: name.into(),
            arguments,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScriptedProvider {
    messages: Vec<ScriptedMessage>,
}

impl ScriptedProvider {
    #[must_use]
    pub fn new(messages: Vec<ScriptedMessage>) -> Self {
        Self { messages }
    }

    #[must_use]
    pub fn echo() -> Self {
        Self::new(vec![ScriptedMessage::text("ok")])
    }
}

impl Provider for ScriptedProvider {
    fn run(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
        mut incoming: mpsc::Receiver<ToolResult>,
    ) -> impl Future<Output = ()> + Send {
        let messages = self.messages.clone();
        let turn_id = request.turn_id;
        async move {
            let mut call_id: crate::tool::ToolCallId = 1;
            for message in messages {
                if cancel.is_cancelled() {
                    let _ = out.send(EngineSignal::Cancelled { turn_id }).await;
                    return;
                }
                match message {
                    ScriptedMessage::Text { delay, text } => {
                        if !delay.is_zero() {
                            tokio::select! {
                                () = cancel.cancelled() => {
                                    let _ = out
                                        .send(EngineSignal::Cancelled { turn_id })
                                        .await;
                                    return;
                                }
                                () = sleep(delay) => {}
                            }
                        }
                        if cancel.is_cancelled() {
                            let _ = out.send(EngineSignal::Cancelled { turn_id }).await;
                            return;
                        }
                        if out
                            .send(EngineSignal::TextDelta { turn_id, text })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    ScriptedMessage::ToolCall { name, arguments } => {
                        let id = call_id;
                        call_id += 1;
                        if out
                            .send(EngineSignal::ToolCallRequest {
                                turn_id,
                                call: ToolCall {
                                    turn_id,
                                    call_id: id,
                                    name,
                                    arguments,
                                },
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                        tokio::select! {
                            result = incoming.recv() => {
                                match result {
                                    Some(ToolResult::Ok { output, .. }) => {
                                        if out
                                            .send(EngineSignal::TextDelta {
                                                turn_id,
                                                text: output,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }
                                    Some(ToolResult::Err { error, .. }) => {
                                        let _ = out
                                            .send(EngineSignal::Failed {
                                                turn_id,
                                                message: error,
                                            })
                                            .await;
                                        return;
                                    }
                                    None => return,
                                }
                            }
                            () = cancel.cancelled() => {
                                let _ = out
                                    .send(EngineSignal::Cancelled { turn_id })
                                    .await;
                                return;
                            }
                        }
                    }
                }
            }

            if cancel.is_cancelled() {
                let _ = out.send(EngineSignal::Cancelled { turn_id }).await;
                return;
            }
            let _ = out.send(EngineSignal::Completed { turn_id }).await;
        }
    }
}
