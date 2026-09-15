use serde::{Deserialize, Serialize};

use crate::{GenerationControls, Message, ModelRef, ToolSpec};

/// One provider call, assembled from a frozen request basis.
///
/// Every field is part of the frozen basis: replaying the same request means
/// sending the same instruction text, messages, tool catalog and generation
/// controls, not re-deriving them from current configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: ModelRef,
    /// The exact instruction text selected when the basis was captured.
    /// `None` means no instruction channel was configured, which is different
    /// from an empty instruction string.
    pub instructions: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub controls: GenerationControls,
}
