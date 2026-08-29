pub(super) use std::future::Future;
pub(super) use std::sync::{Arc, Mutex};
pub(super) use std::time::{Duration, Instant};

pub(super) use serde_json::json;
pub(super) use tokio::sync::mpsc;
pub(super) use tokio::time::{sleep, timeout};
pub(super) use tokio_util::sync::CancellationToken;

pub(super) use crate::context::{ContextMessage, ContextPlan, load_trusted_resources};
pub(super) use crate::error::{CommandError, RuntimeError};
pub(super) use crate::ids::{EffectId, InboxId, OperationId};
pub(super) use crate::operation::{
    Applied, EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome,
    OperationState, SessionEntry, Transition,
};
pub(super) use crate::policy::{AllowlistPolicy, PolicyEngine};
pub(super) use crate::provider::{
    EngineSignal, Provider, ProviderRequest, ScriptedMessage, ScriptedProvider,
};
pub(super) use crate::runtime::{
    EffectBoundary, EffectGate, OperationStatus, Runtime, RuntimeEvent, SaturatedHandle,
    SessionHandle,
};
pub(super) use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EntryRecord, InboxRecord, SessionRecord,
    SessionStore,
};
pub(super) use crate::tool::{
    Tool, ToolCatalog, ToolOutcome, ToolRegistry, ToolSpec, registry_with_policy_route,
};
pub(super) use crate::{ModelConfig, ModelKind, TokenUsage};

pub(super) fn start_runtime(
    provider: impl Provider,
    tools: impl Into<ToolCatalog>,
) -> Runtime {
    Runtime::start_with_store(
        provider,
        tools,
        SessionStore::open_in_memory().expect("store"),
    )
}

pub(super) fn start_runtime_with_store(
    provider: impl Provider,
    tools: impl Into<ToolCatalog>,
    store: SessionStore,
) -> Runtime {
    Runtime::start_with_store(provider, tools, store)
}

pub(super) fn permissive_policy() -> Arc<dyn PolicyEngine> {
    Arc::new(AllowlistPolicy::new([
        "bash", "write", "edit", "read", "search", "find", "agents",
    ]))
}

pub(super) fn step_model() -> ModelConfig {
    ModelConfig {
        model_ref: "test-model".to_owned(),
        context_window: None,
        capabilities: crate::ModelCapabilities::default(),
    }
}

pub(super) async fn collect_until_terminal(
    events: &mut tokio::sync::broadcast::Receiver<RuntimeEvent>,
) -> Result<Vec<RuntimeEvent>, String> {
    let mut recorded = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .map_err(|_| "timed out waiting for terminal event".to_owned())?
            .map_err(|err| err.to_string())?;
        let terminal = matches!(
            event,
            RuntimeEvent::OperationFinished { .. } | RuntimeEvent::OperationFailed { .. }
        );
        recorded.push(event);
        if terminal {
            return Ok(recorded);
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct SharedLogProvider {
    pub(super) log: Arc<Mutex<Vec<ProviderRequest>>>,
    pub(super) settle_delay: Duration,
}

impl SharedLogProvider {
    pub(super) fn requests(&self) -> Vec<ProviderRequest> {
        self.log.lock().expect("log").clone()
    }
}

impl Provider for SharedLogProvider {
    fn complete_stream<'a>(
        &'a self,
        request: ProviderRequest,
        tx: mpsc::Sender<EngineSignal>,
        cancel: CancellationToken,
    ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.log.lock().expect("log").push(request);
            if self.settle_delay > Duration::ZERO {
                tokio::select! {
                    () = sleep(self.settle_delay) => {}
                    () = cancel.cancelled() => {
                        let _ = tx.send(EngineSignal::Failed("cancelled".to_owned())).await;
                        return;
                    }
                }
            }
            if cancel.is_cancelled() {
                let _ = tx.send(EngineSignal::Failed("cancelled".to_owned())).await;
                return;
            }
            let _ = tx
                .send(EngineSignal::TextDelta("working".to_owned()))
                .await;
            let _ = tx
                .send(EngineSignal::Completed {
                    usage: TokenUsage::default(),
                })
                .await;
        })
    }
}

#[derive(Clone, Default)]
pub(super) struct RecordingPolicy {
    calls: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
}

impl RecordingPolicy {
    pub(super) fn calls(&self) -> Vec<(String, serde_json::Value)> {
        self.calls.lock().expect("calls").clone()
    }
}

impl PolicyEngine for RecordingPolicy {
    fn decide(&self, tool: &str, arguments: &serde_json::Value) -> crate::PolicyDecision {
        self.calls
            .lock()
            .expect("calls")
            .push((tool.to_owned(), arguments.clone()));
        crate::PolicyDecision::Allow
    }
}

#[derive(Clone)]
pub(super) struct WindowProvider {
    pub(super) window: u64,
    pub(super) responses: Arc<Mutex<Vec<ScriptedMessage>>>,
    pub(super) requests: Arc<Mutex<Vec<ProviderRequest>>>,
}

impl WindowProvider {
    pub(super) fn new(window: u64, responses: Vec<ScriptedMessage>) -> Self {
        Self {
            window,
            responses: Arc::new(Mutex::new(responses)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Provider for WindowProvider {
    fn complete_stream<'a>(
        &'a self,
        request: ProviderRequest,
        tx: mpsc::Sender<EngineSignal>,
        _cancel: CancellationToken,
    ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.requests.lock().expect("requests").push(request);
            let message = {
                let mut responses = self.responses.lock().expect("responses");
                if responses.is_empty() {
                    ScriptedMessage::text("done")
                } else {
                    responses.remove(0)
                }
            };
            match message {
                ScriptedMessage::Text(text) => {
                    let _ = tx.send(EngineSignal::TextDelta(text)).await;
                    let _ = tx
                        .send(EngineSignal::Completed {
                            usage: TokenUsage::default(),
                        })
                        .await;
                }
                ScriptedMessage::Tool { name, arguments } => {
                    let _ = tx
                        .send(EngineSignal::ToolCallDelta {
                            index: 0,
                            call_id: "test-call".to_owned(),
                            name: Some(name),
                            arguments: Some(arguments.to_string()),
                        })
                        .await;
                    let _ = tx
                        .send(EngineSignal::Completed {
                            usage: TokenUsage::default(),
                        })
                        .await;
                }
                ScriptedMessage::Failure(message) => {
                    let _ = tx.send(EngineSignal::Failed(message)).await;
                }
            }
        })
    }

    fn context_window_for<'a>(
        &'a self,
        _model_ref: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Option<u64>> + Send + 'a>> {
        Box::pin(async move { Some(self.window) })
    }
}

#[derive(Clone)]
pub(super) struct CompactionProbe {
    inner: WindowProvider,
}

impl CompactionProbe {
    pub(super) fn with_window(window: u64) -> Self {
        Self {
            inner: WindowProvider::new(
                window,
                vec![
                    ScriptedMessage::text("first"),
                    ScriptedMessage::text("summary"),
                    ScriptedMessage::text("final"),
                ],
            ),
        }
    }
}

impl Provider for CompactionProbe {
    fn complete_stream<'a>(
        &'a self,
        request: ProviderRequest,
        tx: mpsc::Sender<EngineSignal>,
        cancel: CancellationToken,
    ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        self.inner.complete_stream(request, tx, cancel)
    }

    fn context_window_for<'a>(
        &'a self,
        model_ref: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Option<u64>> + Send + 'a>> {
        self.inner.context_window_for(model_ref)
    }
}
