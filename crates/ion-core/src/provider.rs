//! Provider port used by the operation engine.
//!
//! One `run` call is one ModelStep effect (DESIGN.md §6, §10.3): project
//! input plus a frozen tool snapshot in, one validated provider
//! generation out. The `SessionRuntime` owns the operation loop; a
//! provider never drives tools itself.

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

/// Provider identity and metadata frozen for one model-step attempt.
/// Recovery must use this exact identity rather than the host's current
/// launch default (DESIGN.md §§6, 11.3, 14.8).
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelConfig {
    pub model_ref: String,
    pub context_window: Option<u64>,
    pub capabilities: ModelCapabilities,
}

/// What one model step asks the provider: the operation it belongs to,
/// the projected input, and the frozen model/capability snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequest {
    pub operation_id: OperationId,
    /// Monotonic model-step counter within the operation. Providers echo
    /// it in every signal so the runtime can drop stale generations.
    pub step: u64,
    /// Exact provider identity and metadata persisted with the effect.
    pub model: ModelConfig,
    /// The deterministic projection of session state for this step
    /// (DESIGN.md §14, §31 invariant 15).
    pub plan: ContextPlan,
    pub tools: Vec<ToolSpec>,
}

/// Signals flowing provider → session runtime for one model step
/// (DESIGN.md §15.1). A provider stream becomes durable semantic
/// assistant content only at a validated completion boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineSignal {
    TextDelta {
        operation_id: OperationId,
        step: u64,
        text: String,
    },
    /// Streamed reasoning text from a reasoning model (OpenRouter
    /// `delta.reasoning`). Display-only: thinking is never buffered
    /// into assistant content or durable entries.
    ThinkingDelta {
        operation_id: OperationId,
        step: u64,
        text: String,
    },
    /// One complete provider-native tool call. Never executed from
    /// partial streamed JSON (DESIGN.md §15.2).
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
    /// Token usage reported by the provider for this step, when it
    /// exposes one. Buffered and persisted at the settlement boundary,
    /// independent of operation success (DESIGN.md §27.2).
    UsageUpdate {
        operation_id: OperationId,
        step: u64,
        usage: TokenUsage,
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

    /// Initial model selection for a newly-created session. Hosts may
    /// choose it, but the session persists it before any model effect.
    fn initial_model_ref(&self) -> String {
        std::any::type_name::<Self>().to_owned()
    }

    /// Whether this composed provider can resolve an exact model id.
    fn supports_model(&self, model_ref: &str) -> bool {
        model_ref == self.initial_model_ref()
    }

    /// Metadata for an exact model id. `None` means unknown: hints
    /// degrade to an absolute threshold and overflow is the backstop.
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

    /// Metadata for this provider's fixed model. Adapters must not guess.
    fn context_window(&self) -> impl Future<Output = Option<u64>> + Send {
        std::future::ready(None)
    }

    /// Capabilities for the provider's fixed model. Defaults are
    /// conservative and adapters override them when they can prove support.
    fn capabilities(&self) -> impl Future<Output = ModelCapabilities> + Send {
        std::future::ready(ModelCapabilities::default())
    }

    /// Capabilities for an exact model id. Unknown models do not grant
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
        request: ProviderRequest,
        cancel: tokio_util::sync::CancellationToken,
        out: mpsc::Sender<EngineSignal>,
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

/// Host-composed provider resolver. SessionRuntime owns the selected
/// model id; each immutable ProviderRequest selects an exact cached
/// provider. Locks protect only the cache and are never held over I/O.
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
        request: ProviderRequest,
        cancel: tokio_util::sync::CancellationToken,
        out: mpsc::Sender<EngineSignal>,
    ) {
        let Some(provider) = self.provider_for(&request.model.model_ref) else {
            let _ = out
                .send(EngineSignal::Failed {
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

/// One scripted model step. A script drives successive steps: the
/// runtime executes admitted tools between steps and starts the next
/// step with the projected continuation.
#[derive(Debug, Clone)]
pub enum ScriptedMessage {
    /// Emit `text` as an assistant text delta. `delay` is waited first
    /// (cancellation-aware).
    Text { delay: Duration, text: String },
    /// Emit `text` as a reasoning delta (display-only surface).
    Thinking { text: String },
    /// Emit one complete tool call, then complete the step. The runtime
    /// runs the tool and starts the next step.
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
    /// Emit a usage update, then continue with the next message.
    Usage(TokenUsage),
    /// Fail the step with the given message.
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

/// A provider adapter that plays a scripted transcript across successive
/// model steps. Each `run` consumes script messages until the step
/// completes: text messages stream as deltas; a tool-call message emits
/// the call and ends the step.
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

    /// Set the model context window the runtime should assume (§14.8).
    #[must_use]
    pub fn with_context_window(mut self, tokens: u64) -> Self {
        self.context_window = Some(tokens);
        self
    }

    /// Mark prompt-cache metrics as supported by this scripted adapter.
    /// Tests use this to exercise cache-expectation diagnostics.
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
        request: ProviderRequest,
        cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
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
                    let _ = out
                        .send(EngineSignal::Completed { operation_id, step })
                        .await;
                    return;
                };
                if cancel.is_cancelled() {
                    let _ = out
                        .send(EngineSignal::Cancelled { operation_id, step })
                        .await;
                    return;
                }
                match message {
                    ScriptedMessage::Text { delay, text } => {
                        if !delay.is_zero() {
                            tokio::select! {
                                () = cancel.cancelled() => {
                                    let _ = out
                                        .send(EngineSignal::Cancelled { operation_id, step })
                                        .await;
                                    return;
                                }
                                () = sleep(delay) => {}
                            }
                        }
                        if cancel.is_cancelled() {
                            let _ = out
                                .send(EngineSignal::Cancelled { operation_id, step })
                                .await;
                            return;
                        }
                        if out
                            .send(EngineSignal::TextDelta {
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
                            .send(EngineSignal::ThinkingDelta {
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
                            .send(EngineSignal::Failed {
                                operation_id,
                                step,
                                message,
                            })
                            .await;
                        return;
                    }
                    ScriptedMessage::Usage(usage) => {
                        if out
                            .send(EngineSignal::UsageUpdate {
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
                            .send(EngineSignal::ToolCallCompleted {
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
                        let _ = out
                            .send(EngineSignal::Completed { operation_id, step })
                            .await;
                        return;
                    }
                }
            }
        }
    }
}
