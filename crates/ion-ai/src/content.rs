use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Content {
    Text(String),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}
