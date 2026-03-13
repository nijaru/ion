//! Shared types for LLM providers.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum StreamEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCall(ToolCallEvent),
    Usage(Usage),
    Done,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Accumulates streamed tool call deltas into a complete `ToolCallEvent`.
///
/// Used by providers that receive tool call arguments as incremental JSON chunks.
/// Anthropic sends id/name upfront; OpenAI-compatible providers send them incrementally.
#[derive(Debug, Default)]
pub struct ToolBuilder {
    pub id: Option<String>,
    pub name: Option<String>,
    parts: Vec<String>,
}

impl ToolBuilder {
    /// Create a builder with id and name already known (Anthropic pattern).
    pub fn with_id_name(id: String, name: String) -> Self {
        Self {
            id: Some(id),
            name: Some(name),
            parts: Vec::new(),
        }
    }

    /// Append a JSON fragment to the arguments buffer.
    pub fn push(&mut self, part: String) {
        self.parts.push(part);
    }

    /// Assemble the final tool call event, parsing the accumulated JSON.
    ///
    /// Returns `None` if id or name is missing.
    pub fn finish(self) -> Option<ToolCallEvent> {
        let id = self.id?;
        let name = self.name?;
        let json_str: String = self.parts.concat();
        let arguments = serde_json::from_str(&json_str).unwrap_or_else(|e| {
            tracing::warn!(
                tool = %name,
                error = %e,
                json_preview = %json_str.chars().take(100).collect::<String>(),
                "Malformed tool arguments JSON, using null"
            );
            serde_json::Value::Null
        });
        Some(ToolCallEvent {
            id,
            name,
            arguments,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
}

/// Response from a non-streaming completion request.
#[derive(Debug, Clone)]
pub struct CompletionResponse {
    pub message: Message,
    pub usage: Usage,
}

/// Pricing per million tokens.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // Model capabilities are naturally boolean flags
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub context_window: u32,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_thinking: bool,
    pub supports_cache: bool,
    pub pricing: ModelPricing,
    pub created: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Arc<Vec<ContentBlock>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    ToolResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_call")]
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_call_id: String,
        content: String,
        is_error: bool,
    },
    #[serde(rename = "image")]
    Image { media_type: String, data: String },
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Arc<Vec<Message>>,
    pub system: Option<Cow<'static, str>>,
    pub tools: Arc<Vec<ToolDefinition>>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub thinking: Option<ThinkingConfig>,
}

#[derive(Debug, Clone)]
pub struct ThinkingConfig {
    pub enabled: bool,
    pub budget_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}
