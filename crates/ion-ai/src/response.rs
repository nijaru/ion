use serde::{Deserialize, Serialize};

use crate::{Message, ToolCall, Usage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub message: Message,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ModelStreamEvent {
    TextDelta(String),
    ToolCall(ToolCall),
    Usage(Usage),
    Completed(ModelResponse),
}
