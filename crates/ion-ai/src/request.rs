use serde::{Deserialize, Serialize};

use crate::{GenerationControls, Message, ModelRoute, Role, ToolSpec};

/// Borrowed transcript role sequence for context-encoding planning without
/// cloning message payloads.
#[derive(Clone, Copy)]
pub enum MessageRoles<'a> {
    Messages(&'a [Message]),
    Roles(&'a [Role]),
}

impl MessageRoles<'_> {
    pub fn get(&self, index: usize) -> Option<Role> {
        match self {
            Self::Messages(messages) => messages.get(index).map(|message| message.role),
            Self::Roles(roles) => roles.get(index).copied(),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Messages(messages) => messages.len(),
            Self::Roles(roles) => roles.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Borrowed planning view of model context evolution for adapter overhead checks.
#[derive(Clone, Copy)]
pub struct ModelContextEncoding<'a> {
    pub instructions: Option<&'a str>,
    pub tools: &'a [ToolSpec],
    pub context_timeline: Option<&'a ModelContextTimeline>,
    pub controls: &'a GenerationControls,
    pub message_roles: MessageRoles<'a>,
}

impl<'a> ModelContextEncoding<'a> {
    pub fn from_request(request: &'a ModelRequest) -> Self {
        Self {
            instructions: request.instructions.as_deref(),
            tools: &request.tools,
            context_timeline: request.context_timeline.as_ref(),
            controls: &request.controls,
            message_roles: MessageRoles::Messages(&request.messages),
        }
    }
}

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
    /// Opaque conversation identity for supported provider routing/cache affinity.
    /// This is transport metadata, never model-visible content or replay state.
    pub provider_session_id: Option<String>,
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
