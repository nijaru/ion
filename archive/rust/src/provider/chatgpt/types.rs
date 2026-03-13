//! ChatGPT Responses API types.

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
    pub store: bool,
    pub stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
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

#[derive(Debug)]
pub(crate) enum ParsedEvent {
    TextDelta(String),
    ToolCall(ToolCallEvent),
    Done,
    Error(String),
}
