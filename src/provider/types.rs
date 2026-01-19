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

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
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
