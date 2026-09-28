use serde::{Deserialize, Serialize};

use crate::{GenerationControls, Message, ModelRef, ToolSpec};

/// One provider call assembled for the current model step.
///
/// The agent selects instructions, messages, tools and controls before sending
/// this value to an adapter. The Session records committed conversation facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: ModelRef,
    /// The instruction text selected for this call.
    /// `None` means no instruction channel was configured, which is different
    /// from an empty instruction string.
    pub instructions: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub controls: GenerationControls,
}
