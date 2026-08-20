//! Provider port used by the operation engine.
//!
//! One `run` call is one ModelStep effect (DESIGN.md §6, §10.3): project
//! input plus a frozen tool snapshot in, one validated provider
//! generation out. The `SessionRuntime` owns the operation loop; a
//! provider never drives tools itself.

use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::ids::OperationId;
use crate::tool::{ToolCall, ToolSpec};

/// What one model step asks the provider: the operation it belongs to,
/// the projected input, and the frozen capability snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequest {
    pub operation_id: OperationId,
    pub prompt: String,
    pub tools: Vec<ToolSpec>,
}

/// Signals flowing provider → session runtime for one model step
/// (DESIGN.md §15.1). A provider stream becomes durable semantic
/// assistant content only at a validated completion boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSignal {
    TextDelta {
        operation_id: OperationId,
        text: String,
    },
    /// One complete provider-native tool call. Never executed from
    /// partial streamed JSON (DESIGN.md §15.2).
    ToolCallCompleted {
        operation_id: OperationId,
        call: ToolCall,
    },
    Completed {
        operation_id: OperationId,
    },
    Failed {
        operation_id: OperationId,
        message: String,
    },
    Cancelled {
        operation_id: OperationId,
    },
    /// The provider's task finished without a terminal signal for its
    /// model step. `step` tags the spawning step so stale sentinels from
    /// earlier steps are ignored.
    ProviderExited {
        operation_id: OperationId,
        step: u64,
    },
}

/// A provider adapter executing one model step per `run` call.
pub trait Provider: Send + Sync + 'static {
    fn run(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
    ) -> impl Future<Output = ()> + Send;
}

/// One scripted model step. A script drives successive steps: the
/// runtime executes admitted tools between steps and starts the next
/// step with the projected continuation.
#[derive(Debug, Clone)]
pub enum ScriptedMessage {
    /// Emit `text` as an assistant text delta. `delay` is waited first
    /// (cancellation-aware).
    Text { delay: Duration, text: String },
    /// Emit one complete tool call, then complete the step. The runtime
    /// runs the tool and starts the next step.
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

/// A provider adapter that plays a scripted transcript across successive
/// model steps. Each `run` consumes script messages until the step
/// completes: text messages stream as deltas; a tool-call message emits
/// the call and ends the step.
#[derive(Debug)]
pub struct ScriptedProvider {
    cursor: Mutex<ScriptCursor>,
    call_ids: AtomicU64,
}

#[derive(Debug)]
struct ScriptCursor {
    next: usize,
    messages: Vec<ScriptedMessage>,
}

impl ScriptedProvider {
    #[must_use]
    pub fn new(messages: Vec<ScriptedMessage>) -> Self {
        Self {
            cursor: Mutex::new(ScriptCursor { next: 0, messages }),
            call_ids: AtomicU64::new(1),
        }
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
    ) -> impl Future<Output = ()> + Send {
        let operation_id = request.operation_id;
        async move {
            loop {
                let message = {
                    let mut cursor = self.cursor.lock().expect("script cursor poisoned");
                    let message = cursor.messages.get(cursor.next).cloned();
                    if message.is_some() {
                        cursor.next += 1;
                    }
                    message
                };
                let Some(message) = message else {
                    let _ = out.send(EngineSignal::Completed { operation_id }).await;
                    return;
                };
                if cancel.is_cancelled() {
                    let _ = out.send(EngineSignal::Cancelled { operation_id }).await;
                    return;
                }
                match message {
                    ScriptedMessage::Text { delay, text } => {
                        if !delay.is_zero() {
                            tokio::select! {
                                () = cancel.cancelled() => {
                                    let _ = out
                                        .send(EngineSignal::Cancelled { operation_id })
                                        .await;
                                    return;
                                }
                                () = sleep(delay) => {}
                            }
                        }
                        if cancel.is_cancelled() {
                            let _ = out.send(EngineSignal::Cancelled { operation_id }).await;
                            return;
                        }
                        if out
                            .send(EngineSignal::TextDelta { operation_id, text })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    ScriptedMessage::ToolCall { name, arguments } => {
                        let call_id = self.call_ids.fetch_add(1, Ordering::Relaxed);
                        if out
                            .send(EngineSignal::ToolCallCompleted {
                                operation_id,
                                call: ToolCall {
                                    operation_id,
                                    call_id,
                                    name,
                                    arguments,
                                },
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                        let _ = out.send(EngineSignal::Completed { operation_id }).await;
                        return;
                    }
                }
            }
        }
    }
}
