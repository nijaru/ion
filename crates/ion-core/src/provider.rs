//! Provider port used by the operation engine.
//!
//! One `run` call is one model-step effect (DESIGN.md §6, §10.3): projected
//! input plus a frozen tool snapshot in, one validated provider generation out.
//! The session owner drives the operation loop; a provider never drives tools.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::context::ContextPlan;
use crate::ids::OperationId;
use crate::tool::{ToolCall, ToolSpec};

/// Token accounting for one model step (DESIGN.md §27.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    /// Tokens served from the provider prompt cache (§14.4).
    pub cache_read: u64,
    /// Tokens written to the provider prompt cache (§14.4).
    pub cache_write: u64,
}

/// Capabilities advertised for an exact resolved model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelCapabilities {
    /// The adapter can expose provider reasoning as a separate live signal.
    pub reasoning: bool,
    /// The adapter can project and accept the frozen tool snapshot.
    pub tool_calls: bool,
    /// The adapter reports prompt-cache metrics with provider semantics.
    pub prompt_cache: bool,
    /// The adapter can stream normalized deltas.
    pub streaming: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            reasoning: false,
            tool_calls: true,
            prompt_cache: false,
            streaming: true,
        }
    }
}

/// Exact provider/model identity and metadata frozen for one model-step
/// attempt. Recovery must use this resolved value rather than a host launch
/// default (DESIGN.md §§6, 11.3, 14.8).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedModel {
    pub model_ref: String,
    pub context_window: Option<u64>,
    pub capabilities: ModelCapabilities,
}

/// Compatibility name retained while callers migrate to [`ResolvedModel`].
pub type ModelConfig = ResolvedModel;

/// Immutable provider input for one model invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    pub operation_id: OperationId,
    /// Monotonic model-step counter within the operation. Providers echo it in
    /// every event so the session owner can drop stale generations.
    pub step: u64,
    /// Exact provider/model identity and metadata persisted with the effect.
    pub model: ResolvedModel,
    /// Deterministic projection of local semantic state for this invocation.
    pub plan: ContextPlan,
    /// Frozen model-facing tool definitions for this invocation.
    pub tools: Vec<ToolSpec>,
}

/// Compatibility name retained while callers migrate to [`ModelRequest`].
pub type ProviderRequest = ModelRequest;

/// Normalized provider → session events for one model invocation.
/// A stream becomes durable assistant content only at a validated completion
/// boundary (DESIGN.md §15.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelEvent {
    TextDelta {
        operation_id: OperationId,
        step: u64,
        text: String,
    },
    /// Streamed provider reasoning. Display-only: it is never promoted to
    /// semantic assistant content merely because it was streamed.
    ThinkingDelta {
        operation_id: OperationId,
        step: u64,
        text: String,
    },
    /// One complete provider-native tool call. Never execute partial streamed
    /// JSON (DESIGN.md §15.2).
    ToolCallCompleted {
        operation_id: OperationId,
        step: u64,
        call: ToolCall,
    },
    Completed {
        operation_id: OperationId,
        step: u64,
    },
    Failed {
        operation_id: OperationId,
        step: u64,
        message: String,
    },
    Cancelled {
        operation_id: OperationId,
        step: u64,
    },
    /// Provider token accounting for this invocation, when available.
    UsageUpdate {
        operation_id: OperationId,
        step: u64,
        usage: TokenUsage,
    },
    /// The provider task returned without a terminal event. `step` correlates
    /// the sentinel so a stale task cannot settle a later invocation.
    ProviderExited {
        operation_id: OperationId,
        step: u64,
    },
}

/// Compatibility name retained while callers migrate to [`ModelEvent`].
pub type EngineSignal = ModelEvent;

/// Provider adapter: one `run` call executes exactly one model invocation.
pub trait Provider: Send + Sync + 'static {
    fn run(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<ModelEvent>,
    ) -> impl Future<Output = ()> + Send;

    /// Initial model selection for a newly-created session. Hosts may choose
    /// it, but the session persists it before any model effect.
    fn initial_model_ref(&self) -> String {
        std::any::type_name::<Self>().to_owned()
    }

    /// Whether this composed provider can resolve an exact model id.
    fn supports_model(&self, model_ref: &str) -> bool {
        model_ref == self.initial_model_ref()
    }

    /// Context capacity for an exact model id. Adapters must not guess.
    fn context_window_for(&self, model_ref: &str) -> impl Future<Output = Option<u64>> + Send {
        let supported = self.supports_model(model_ref);
        async move {
            if supported {
                self.context_window().await
            } else {
                None
            }
        }
    }

    /// Context capacity for this provider's fixed model.
    fn context_window(&self) -> impl Future<Output = Option<u64>> + Send {
        std::future::ready(None)
    }

    /// Capabilities for this provider's fixed model. Defaults are conservative.
    fn capabilities(&self) -> impl Future<Output = ModelCapabilities> + Send {
        std::future::ready(ModelCapabilities::default())
    }

    /// Capabilities for an exact model id. Unknown models never inherit
    /// capabilities merely because a host requested them.
    fn capabilities_for(&self, model_ref: &str) -> impl Future<Output = ModelCapabilities> + Send {
        let supported = self.supports_model(model_ref);
        async move {
            if supported {
                self.capabilities().await
            } else {
                ModelCapabilities::default()
            }
        }
    }
}

impl<P: Provider> Provider for Arc<P> {
    async fn run(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<ModelEvent>,
    ) {
        (**self).run(request, cancel, out).await
    }

    fn initial_model_ref(&self) -> String {
        (**self).initial_model_ref()
    }

    fn supports_model(&self, model_ref: &str) -> bool {
        (**self).supports_model(model_ref)
    }

    async fn context_window_for(&self, model_ref: &str) -> Option<u64> {
        (**self).context_window_for(model_ref).await
    }

    async fn context_window(&self) -> Option<u64> {
        (**self).context_window().await
    }

    async fn capabilities(&self) -> ModelCapabilities {
        (**self).capabilities().await
    }

    async fn capabilities_for(&self, model_ref: &str) -> ModelCapabilities {
        (**self).capabilities_for(model_ref).await
    }
}

/// Host-composed model resolver. The session owner owns model selection; each
/// immutable [`ModelRequest`] selects an exact cached provider. Locks protect
/// only the cache and are never held over I/O.
pub struct SwitchingProvider<P: Provider> {
    initial_model: String,
    providers: Mutex<HashMap<String, Arc<P>>>,
    make: Option<Arc<dyn Fn(String) -> P + Send + Sync>>,
}

impl<P: Provider> SwitchingProvider<P> {
    /// A fixed provider. It accepts only `model` and cannot switch.
    #[must_use]
    pub fn new(model: impl Into<String>, provider: P) -> Self {
        let model = model.into();
        let mut providers = HashMap::new();
        providers.insert(model.clone(), Arc::new(provider));
        Self {
            initial_model: model,
            providers: Mutex::new(providers),
            make: None,
        }
    }

    /// A model resolver for a session whose selection may change.
    #[must_use]
    pub fn switchable(
        model: impl Into<String>,
        provider: P,
        make: Arc<dyn Fn(String) -> P + Send + Sync>,
    ) -> Self {
        let model = model.into();
        let mut providers = HashMap::new();
        providers.insert(model.clone(), Arc::new(provider));
        Self {
            initial_model: model,
            providers: Mutex::new(providers),
            make: Some(make),
        }
    }

    fn provider_for(&self, model_ref: &str) -> Option<Arc<P>> {
        let mut providers = self.providers.lock().expect("provider cache poisoned");
        if let Some(provider) = providers.get(model_ref) {
            return Some(Arc::clone(provider));
        }
        let provider = Arc::new((self.make.as_ref()?)(model_ref.to_owned()));
        providers.insert(model_ref.to_owned(), Arc::clone(&provider));
        Some(provider)
    }
}

impl<P: Provider> Provider for SwitchingProvider<P> {
    async fn run(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<ModelEvent>,
    ) {
        let Some(provider) = self.provider_for(&request.model.model_ref) else {
            let _ = out
                .send(ModelEvent::Failed {
                    operation_id: request.operation_id,
                    step: request.step,
                    message: format!("model {} is unavailable", request.model.model_ref),
                })
                .await;
            return;
        };
        provider.run(request, cancel, out).await;
    }

    fn initial_model_ref(&self) -> String {
        self.initial_model.clone()
    }

    fn supports_model(&self, model_ref: &str) -> bool {
        self.make.is_some()
            || self
                .providers
                .lock()
                .expect("provider cache poisoned")
                .contains_key(model_ref)
    }

    async fn context_window_for(&self, model_ref: &str) -> Option<u64> {
        let provider = self.provider_for(model_ref)?;
        provider.context_window().await
    }

    async fn capabilities_for(&self, model_ref: &str) -> ModelCapabilities {
        let Some(provider) = self.provider_for(model_ref) else {
            return ModelCapabilities::default();
        };
        provider.capabilities().await
    }
}

/// One scripted model-step action used by deterministic tests.
#[derive(Debug, Clone)]
pub enum ScriptedMessage {
    Text { delay: Duration, text: String },
    Thinking { text: String },
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
    Usage(TokenUsage),
    Fail { message: String },
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

/// Deterministic provider that plays a scripted transcript across successive
/// model invocations.
#[derive(Debug)]
pub struct ScriptedProvider {
    cursor: Mutex<ScriptCursor>,
    call_ids: AtomicU64,
    context_window: Option<u64>,
    prompt_cache: bool,
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
            context_window: None,
            prompt_cache: false,
        }
    }

    #[must_use]
    pub fn with_context_window(mut self, tokens: u64) -> Self {
        self.context_window = Some(tokens);
        self
    }

    #[must_use]
    pub fn with_prompt_cache(mut self, enabled: bool) -> Self {
        self.prompt_cache = enabled;
        self
    }

    #[must_use]
    pub fn echo() -> Self {
        Self::new(vec![ScriptedMessage::text("ok")])
    }
}

impl Provider for ScriptedProvider {
    async fn context_window(&self) -> Option<u64> {
        self.context_window
    }

    async fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            reasoning: true,
            prompt_cache: self.prompt_cache,
            ..ModelCapabilities::default()
        }
    }

    fn run(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<ModelEvent>,
    ) -> impl Future<Output = ()> + Send {
        let operation_id = request.operation_id;
        let step = request.step;
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
                    let _ = out.send(ModelEvent::Completed { operation_id, step }).await;
                    return;
                };
                if cancel.is_cancelled() {
                    let _ = out.send(ModelEvent::Cancelled { operation_id, step }).await;
                    return;
                }
                match message {
                    ScriptedMessage::Text { delay, text } => {
                        if !delay.is_zero() {
                            tokio::select! {
                                () = cancel.cancelled() => {
                                    let _ = out.send(ModelEvent::Cancelled { operation_id, step }).await;
                                    return;
                                }
                                () = sleep(delay) => {}
                            }
                        }
                        if cancel.is_cancelled() {
                            let _ = out.send(ModelEvent::Cancelled { operation_id, step }).await;
                            return;
                        }
                        if out
                            .send(ModelEvent::TextDelta {
                                operation_id,
                                step,
                                text,
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    ScriptedMessage::Thinking { text } => {
                        if out
                            .send(ModelEvent::ThinkingDelta {
                                operation_id,
                                step,
                                text,
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    ScriptedMessage::Fail { message } => {
                        let _ = out
                            .send(ModelEvent::Failed {
                                operation_id,
                                step,
                                message,
                            })
                            .await;
                        return;
                    }
                    ScriptedMessage::Usage(usage) => {
                        if out
                            .send(ModelEvent::UsageUpdate {
                                operation_id,
                                step,
                                usage,
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    ScriptedMessage::ToolCall { name, arguments } => {
                        let call_id = self.call_ids.fetch_add(1, Ordering::Relaxed);
                        if out
                            .send(ModelEvent::ToolCallCompleted {
                                operation_id,
                                step,
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
                        let _ = out.send(ModelEvent::Completed { operation_id, step }).await;
                        return;
                    }
                }
            }
        }
    }
}
