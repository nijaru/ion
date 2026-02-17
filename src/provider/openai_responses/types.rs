//! OpenAI Responses API types.

use crate::provider::types::ToolCallEvent;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(crate) struct ResponsesRequest {
    pub model: String,
    pub instructions: String,
    pub input: Vec<ResponseInputItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    pub store: bool,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextConfig>,
    pub truncation: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponseInputItem {
    Message {
        role: String,
        content: Vec<ResponseContent>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponseContent {
    InputText { text: String },
    OutputText { text: String },
    InputImage { image_url: String },
}

#[derive(Debug, Serialize)]
pub(crate) struct Reasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TextConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<TextFormat>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TextFormat {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Debug)]
pub(crate) enum ParsedEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCallDelta {
        call_id: String,
        delta: String,
    },
    ToolCallDone {
        call_id: String,
        name: String,
        arguments: String,
    },
    ToolCall(ToolCallEvent),
    Usage(crate::provider::types::Usage),
    Done,
    Error(String),
}
