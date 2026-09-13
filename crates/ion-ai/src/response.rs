use serde::{Deserialize, Serialize};

use crate::{Message, ToolCall, Usage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub message: Message,
    pub usage: Usage,
    /// A finished transport stream is not necessarily a complete answer.
    pub termination: ResponseTermination,
}

impl ModelResponse {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(self.termination, ResponseTermination::Completed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseTermination {
    /// The provider produced a complete response.
    Completed,
    /// The provider ended the response before a complete answer was produced.
    /// A generation task must not treat this as a successful final answer.
    Incomplete(IncompleteReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncompleteReason {
    MaxOutputTokens,
    ContextLength,
    ContentFilter,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ModelStreamEvent {
    TextDelta(String),
    ToolCall(ToolCall),
    Usage(Usage),
    Completed(ModelResponse),
}
