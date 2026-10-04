use serde::{Deserialize, Serialize};

use crate::{GenerationControls, Message, ModelRoute, ToolSpec};

/// Provider-neutral model-visible state associated with one request boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelContextState {
    pub instructions: Option<String>,
    pub tools: Vec<ToolSpec>,
}

/// A full context snapshot that takes effect after `after_message` neutral
/// transcript messages and before the following assistant generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelContextChange {
    pub after_message: usize,
    pub context: ModelContextState,
}

/// Reconstructible context evolution for adapters that can preserve an older
/// provider prefix while applying later context changes natively.
///
/// `initial` is the state of the first model request in the projected history.
/// `changes` are full snapshots, not provider-specific deltas. The latest
/// snapshot must agree with `ModelRequest.instructions/tools`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelContextTimeline {
    pub initial: ModelContextState,
    pub changes: Vec<ModelContextChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheIntent {
    /// Do not add provider-specific opt-in caching controls. A provider may
    /// still apply its own transparent caching behavior.
    Default,
    /// This request belongs to a reusable coding conversation; a provider
    /// adapter may opt into prompt caching when the resolved route supports it.
    Reusable,
}

/// One provider call assembled for the current model step.
///
/// The agent selects instructions, messages, tools and controls before sending
/// this value to an adapter. The Session records committed conversation facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    /// Logical selection, effective physical target and why this request exists.
    /// Direct models use the same identity for logical and effective.
    pub route: ModelRoute,
    /// The instruction text selected for this call.
    /// `None` means no instruction channel was configured, which is different
    /// from an empty instruction string.
    pub instructions: Option<String>,
    pub messages: Vec<Message>,
    /// Latest effective provider-neutral tool loadout. Adapters that cannot use
    /// `context_timeline` send this complete list directly.
    pub tools: Vec<ToolSpec>,
    /// Optional historical context evolution. It is an optimization input, not
    /// a second source of conversation truth.
    #[serde(default)]
    pub context_timeline: Option<ModelContextTimeline>,
    pub prompt_cache: PromptCacheIntent,
    pub controls: GenerationControls,
}
