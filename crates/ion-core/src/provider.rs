//! Provider port used by the turn engine.

use std::future::Future;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::ids::TurnId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequest {
    pub turn_id: TurnId,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSignal {
    TextDelta { turn_id: TurnId, text: String },
    Completed { turn_id: TurnId },
    Failed { turn_id: TurnId, message: String },
    Cancelled { turn_id: TurnId },
    ProviderExited { turn_id: TurnId },
}

pub trait Provider: Send + Sync + 'static {
    fn run(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
    ) -> impl Future<Output = ()> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedChunk {
    pub text: String,
    pub delay: Duration,
}

impl ScriptedChunk {
    #[must_use]
    pub fn immediate(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            delay: Duration::ZERO,
        }
    }

    #[must_use]
    pub fn after(delay: Duration, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            delay,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptedProvider {
    chunks: Vec<ScriptedChunk>,
}

impl ScriptedProvider {
    #[must_use]
    pub fn new(chunks: Vec<ScriptedChunk>) -> Self {
        Self { chunks }
    }

    #[must_use]
    pub fn echo() -> Self {
        Self::new(vec![ScriptedChunk::immediate("ok")])
    }
}

impl Provider for ScriptedProvider {
    fn run(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
    ) -> impl Future<Output = ()> + Send {
        let chunks = self.chunks.clone();
        async move {
            for chunk in chunks {
                if cancel.is_cancelled() {
                    let _ = out
                        .send(EngineSignal::Cancelled {
                            turn_id: request.turn_id,
                        })
                        .await;
                    return;
                }
                if !chunk.delay.is_zero() {
                    tokio::select! {
                        () = cancel.cancelled() => {
                            let _ = out
                                .send(EngineSignal::Cancelled {
                                    turn_id: request.turn_id,
                                })
                                .await;
                            return;
                        }
                        () = sleep(chunk.delay) => {}
                    }
                }
                if cancel.is_cancelled() {
                    let _ = out
                        .send(EngineSignal::Cancelled {
                            turn_id: request.turn_id,
                        })
                        .await;
                    return;
                }
                if out
                    .send(EngineSignal::TextDelta {
                        turn_id: request.turn_id,
                        text: chunk.text,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }

            if cancel.is_cancelled() {
                let _ = out
                    .send(EngineSignal::Cancelled {
                        turn_id: request.turn_id,
                    })
                    .await;
                return;
            }
            let _ = out
                .send(EngineSignal::Completed {
                    turn_id: request.turn_id,
                })
                .await;
        }
    }
}
