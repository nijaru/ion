//! Provider-neutral canonical transcript content.
//!
//! Durable history links calls and results by Ion ToolInvocationId. Provider wire ids
//! are replay metadata only and may be remapped by a target adapter.

use ion_ai::ProviderReplay;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::InvocationId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptRole {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptMessage {
    pub role: TranscriptRole,
    pub content: Vec<TranscriptContent>,
    pub provider_replay: Option<ProviderReplay>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TranscriptContent {
    Text(String),
    ToolCall {
        invocation: InvocationId,
        name: String,
        arguments: Value,
        /// Original provider id for compatible replay only.
        origin_provider_id: Option<String>,
    },
    ToolResult {
        invocation: InvocationId,
        name: String,
        result: Value,
    },
}

impl TranscriptMessage {
    #[must_use]
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: TranscriptRole::User,
            content: vec![TranscriptContent::Text(text.into())],
            provider_replay: None,
        }
    }
}
