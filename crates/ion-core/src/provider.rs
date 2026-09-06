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
use crate::ids::{OperationId, SessionId};
use crate::tool::{ToolCall, ToolSpec};

/// Token accounting for one model step (DESIGN.md §27.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    /// Fresh input tokens, excluding the cache-read/write buckets below.
    /// The four fields are disjoint and may be summed for context accounting.
    pub input: u64,
    pub output: u64,
    /// Tokens served from the provider prompt cache (§14.4).
    pub cache_read: u64,
    /// Tokens written to the provider prompt cache (§14.4).
    pub cache_write: u64,
}

impl TokenUsage {
    /// Total tokens occupying model context. Provider counters are external
    /// input, so overflow saturates instead of panicking or wrapping.
    pub(crate) fn context_tokens(self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
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
    /// The model accepts image content (pi parity: `model.input`
    /// includes "image"). Non-vision models see a note instead of the
    /// image bytes.
    pub images: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            reasoning: false,
            tool_calls: true,
            prompt_cache: false,
            streaming: true,
            images: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelConfig {
    pub model_ref: String,
    /// Reasoning-effort selection frozen for this step (pi-parity
    /// thinking level). `None` keeps the adapter default.
    pub thinking: Option<String>,
    pub context_window: Option<u64>,
    pub capabilities: ModelCapabilities,
    /// Per-million-token pricing for the current model step, when the
    /// provider publishes it (pi-parity cost footer). `None` hides the
    /// cost segment instead of guessing.
    pub pricing: Option<ModelPricing>,
}

/// Per-million-token USD prices for one model (pi-ai catalog values).
/// Prices are exact integer micro-dollars so durable model configs keep
/// `Eq` semantics (floats are rejected by the derives) and comparisons
/// stay deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelPricing {
    /// Micro-dollars per one million input tokens ($0.075 → 75_000).
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
}

impl ModelPricing {
    /// USD cost of one model step from its token usage, saturating like
    /// the token counters (provider data is external input).
    pub fn cost_usd(self, usage: TokenUsage) -> f64 {
        let micro = self.input as u64 * usage.input
            + self.output as u64 * usage.output
            + self.cache_read as u64 * usage.cache_read
            + self.cache_write as u64 * usage.cache_write;
        // micro-dollars per million tokens → dollars: divide by 1e6.
        micro as f64 / 1e12
    }
}

/// Bounded retry for transient provider failures (pi-parity
/// `settings.retry`: enabled, 3 attempts, 2s exponential base).
/// Retries wrap the raw provider stream, never the settled operation:
/// a retried attempt replaces the failed stream's draft, and the final
/// terminal signal settles the step exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    pub enabled: bool,
    pub max_retries: u32,
    /// Base delay in ms; per-attempt delay is `base * 2^(attempt-1)`.
    pub base_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 2000,
        }
    }
}

impl RetryPolicy {
    /// Per-attempt backoff delay: `base * 2^(attempt-1)`
    /// (attempt is 1-indexed), saturating like the counters.
    #[must_use]
    pub fn delay_ms(&self, attempt: u32) -> u64 {
        self.base_delay_ms
            .saturating_mul(1_u64 << (attempt - 1).min(63))
    }
}

/// Run one model step with bounded transient-failure retry (pi's
/// `retryAssistantCall`). Each attempt runs the provider against a
/// fresh attempt channel; every signal forwards live to `out` except a
/// retryable terminal `Failed`, which becomes a non-terminal
/// `RetryScheduled` for the runtime (the sole mutation authority: it
/// clears the failed attempt's partial draft before the next attempt
/// runs). Non-retryable failures, exhausted budgets, cancels, and
/// completions forward unchanged. The exit sentinel is emitted exactly
/// once, after the loop ends.
///
/// Context overflow bypasses retry: the runtime's compaction path owns
/// that classification at settlement, and a retry would only burn the
/// same overflowing request again.
pub(crate) async fn run_with_retry<P: Provider>(
    provider: Arc<P>,
    request: ProviderRequest,
    cancel: CancellationToken,
    out: mpsc::Sender<EngineSignal>,
    retry: RetryPolicy,
    terminal: mpsc::Sender<EngineSignal>,
) {
    let max_attempts = if retry.enabled { retry.max_retries } else { 0 };
    let mut attempt = 0_u32;
    loop {
        let (attempt_tx, mut attempt_rx) = mpsc::channel::<EngineSignal>(64);
        {
            let request = request.clone();
            let cancel = cancel.clone();
            let provider = Arc::clone(&provider);
            tokio::spawn(async move {
                provider.run(request, cancel, attempt_tx).await;
            });
        }
        let mut failure: Option<String> = None;
        while let Some(signal) = attempt_rx.recv().await {
            match signal {
                EngineSignal::Failed { message, .. }
                    if attempt < max_attempts
                        && is_retryable_provider_error(&message)
                        && !super::runtime::is_context_overflow(&message)
                        && !cancel.is_cancelled() =>
                {
                    failure = Some(message);
                }
                signal => {
                    let _ = out.send(signal).await;
                }
            }
        }
        let (operation_id, step) = (request.operation_id, request.step);
        let Some(message) = failure else {
            let _ = terminal
                .send(EngineSignal::ProviderExited { operation_id, step })
                .await;
            return;
        };
        attempt += 1;
        let delay = std::time::Duration::from_millis(retry.delay_ms(attempt));
        let _ = out
            .send(EngineSignal::RetryScheduled {
                operation_id,
                step,
                attempt,
                max_attempts,
                delay_ms: delay.as_millis() as u64,
                message,
            })
            .await;
        let slept = tokio::select! {
            () = cancel.cancelled() => false,
            () = tokio::time::sleep(delay) => true,
        };
        if !slept {
            // Cancel during backoff lands as a cancelled step, matching
            // pi's abort normalization: a retry interrupted mid-wait
            // never continues as a partial turn.
            let _ = out
                .send(EngineSignal::Cancelled { operation_id, step })
                .await;
            let _ = terminal
                .send(EngineSignal::ProviderExited { operation_id, step })
                .await;
            return;
        }
    }
}

/// Quota/billing exhaustion patterns: deterministic account limits
/// that no amount of waiting clears (pi's
/// NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN).
#[must_use]
pub fn is_non_retryable_limit_error(message: &str) -> bool {
    let lowered = message.to_lowercase();
    [
        "gousagelimiterror",
        "freeusagelimiterror",
        "monthly usage limit reached",
        "available balance",
        "insufficient_quota",
        "out of budget",
        "quota exceeded",
        "billing",
    ]
    .iter()
    .any(|pattern| lowered.contains(pattern))
}

/// Transient provider/transport failures worth retrying (pi's
/// RETRYABLE_PROVIDER_ERROR_PATTERN: overload, 429/5xx throttles,
/// network and stream drops).
#[must_use]
pub fn is_retryable_provider_error(message: &str) -> bool {
    if is_non_retryable_limit_error(message) {
        return false;
    }
    let lowered = message.to_lowercase();
    [
        "overloaded",
        "rate limit",
        "rate-limit",
        "ratelimit",
        "too many requests",
        "429",
        "500",
        "502",
        "503",
        "504",
        "524",
        "service unavailable",
        "server error",
        "internal error",
        "provider returned error",
        "network error",
        "connection error",
        "connection refused",
        "connection lost",
        "other side closed",
        "fetch failed",
        "getaddrinfo",
        "enotfound",
        "eai_again",
        "upstream connect",
        "reset before headers",
        "socket hang up",
        "socket connection was closed",
        "timed out",
        "timed-out",
        "timeout",
        "terminated",
        "websocket closed",
        "websocket error",
        "stream ended before",
        "ended without",
        "retry delay",
        "you can retry your request",
        "try your request again",
        "please retry your request",
        "resourceexhausted",
        // Rust/reqwest transport wording (the root-cause unwrap keeps
        // the specific OS error in the message; this catches adapters
        // that only format the generic Display).
        "error sending request",
    ]
    .iter()
    .any(|pattern| lowered.contains(pattern))
}

/// What one model step asks the provider: the operation it belongs to,
/// the projected input, and the frozen model/capability snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequest {
    pub operation_id: OperationId,
    /// Monotonic model-step counter within the operation. Providers echo
    /// it in every signal so the runtime can drop stale generations.
    pub step: u64,
    /// Owning session, for provider-side session attribution (pi's
    /// pi-openrouter-session extension: OpenRouter groups request
    /// activity by `session_id` in the request body).
    pub session_id: SessionId,
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
    /// A transient failure was classified retryable and the next
    /// attempt is scheduled after `delay_ms` (the engine's bounded
    /// retry, pi's `retryAssistantCall`). Non-terminal: the runtime
    /// clears the failed attempt's partial draft and keeps the step
    /// open; the exit sentinel still ends the step exactly once.
    RetryScheduled {
        operation_id: OperationId,
        step: u64,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        message: String,
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

    /// Published per-million-token pricing for an exact model id.
    /// `None` means unknown: the cost footer hides the cost segment
    /// instead of guessing (adapters must not invent prices).
    fn pricing_for(&self, model_ref: &str) -> impl Future<Output = Option<ModelPricing>> + Send {
        let supported = self.supports_model(model_ref);
        async move {
            if supported {
                self.pricing().await
            } else {
                None
            }
        }
    }

    /// Pricing for the provider's fixed model. Adapters without published
    /// prices keep the default `None`.
    fn pricing(&self) -> impl Future<Output = Option<ModelPricing>> + Send {
        std::future::ready(None)
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

    async fn pricing_for(&self, model_ref: &str) -> Option<ModelPricing> {
        (**self).pricing_for(model_ref).await
    }

    async fn pricing(&self) -> Option<ModelPricing> {
        (**self).pricing().await
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

    #[must_use]
    pub fn fail(message: impl Into<String>) -> Self {
        Self::Fail {
            message: message.into(),
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

#[cfg(test)]
mod token_usage_tests {
    use super::TokenUsage;

    #[test]
    fn context_tokens_saturate_external_counters() {
        let usage = TokenUsage {
            input: u64::MAX,
            output: 1,
            cache_read: u64::MAX,
            cache_write: u64::MAX,
        };
        assert_eq!(usage.context_tokens(), u64::MAX);
    }
}

#[cfg(test)]
mod retry_tests {
    use super::{RetryPolicy, is_non_retryable_limit_error, is_retryable_provider_error};

    #[test]
    fn retry_defaults_match_pi() {
        let policy = RetryPolicy::default();
        assert!(policy.enabled);
        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.base_delay_ms, 2000);
    }

    #[test]
    fn backoff_doubles_per_attempt_and_saturates() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay_ms(1), 2000);
        assert_eq!(policy.delay_ms(2), 4000);
        assert_eq!(policy.delay_ms(3), 8000);
        // Exponents past 63 saturate instead of panicking.
        let _ = policy.delay_ms(70);
    }

    #[test]
    fn transient_provider_errors_are_retryable() {
        assert!(is_retryable_provider_error(
            "provider returned 429: rate limit exceeded"
        ));
        assert!(is_retryable_provider_error("503 Service Unavailable"));
        assert!(is_retryable_provider_error(
            "provider request failed: connection refused"
        ));
        assert!(is_retryable_provider_error(
            "stream ended before message_stop"
        ));
        assert!(is_retryable_provider_error("request timed out"));
    }

    #[test]
    fn quota_and_billing_errors_fail_fast() {
        assert!(!is_retryable_provider_error(
            "OpenAI: insufficient_quota, billing hard limit reached"
        ));
        assert!(!is_retryable_provider_error("quota exceeded for this key"));
        assert!(!is_non_retryable_limit_error("transient 503, not a limit"));
        assert!(is_non_retryable_limit_error("monthly usage limit reached"));
    }

    #[test]
    fn overflow_messages_are_not_mistaken_for_quota_limits() {
        // Overflow is handled by the runtime's compaction path via the
        // explicit `is_context_overflow` check, not by this classifier;
        // it must never be classified as a deterministic account limit,
        // which would hide it from that path.
        assert!(!is_non_retryable_limit_error("context length exceeded"));
        assert!(!is_non_retryable_limit_error(
            "prompt is too long: too many tokens"
        ));
    }
}
