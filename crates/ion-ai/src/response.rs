use serde::{Deserialize, Serialize};

use crate::{Content, Message, Usage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub message: Message,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ModelStreamEvent {
    TextDelta(String),
    ToolCall(Content),
    Usage(Usage),
    Completed(ModelResponse),
}
