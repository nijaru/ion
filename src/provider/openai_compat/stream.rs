//! OpenAI-compatible streaming response types.

use serde::Deserialize;

/// Streaming chunk from OpenAI-compatible APIs.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct StreamChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<StreamUsage>,
}

/// A choice in a streaming chunk.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct StreamChoice {
    pub index: usize,
    pub delta: StreamDelta,
    pub finish_reason: Option<String>,
}

/// Delta content in a streaming choice.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct StreamDelta {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
    /// Reasoning content (`DeepSeek`, Kimi via `OpenRouter`).
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Alternative reasoning field.
    #[serde(default)]
    pub reasoning: Option<String>,
}

/// Tool call delta in streaming.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct StreamToolCall {
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type", default)]
    pub call_type: Option<String>,
    #[serde(default)]
    pub function: Option<StreamFunctionCall>,
}

/// Function call delta in streaming.
#[derive(Debug, Clone, Deserialize)]
pub struct StreamFunctionCall {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

/// Usage info in streaming (sent at end).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code, clippy::struct_field_names)] // Field names match API response
pub struct StreamUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

/// Detailed prompt token breakdown (OpenAI).
#[derive(Debug, Clone, Deserialize)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_delta() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": {
                    "content": "Hello"
                },
                "finish_reason": null
            }]
        }"#;

        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn test_role_delta() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant"
                },
                "finish_reason": null
            }]
        }"#;

        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.choices[0].delta.role.as_deref(), Some("assistant"));
    }

    #[test]
    fn test_tool_call_delta() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_456",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null
            }]
        }"#;

        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        let tool_calls = chunk.choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls[0].id.as_deref(), Some("call_456"));
        assert_eq!(
            tool_calls[0].function.as_ref().unwrap().name.as_deref(),
            Some("read_file")
        );
    }

    #[test]
    fn test_tool_call_argument_delta() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": "{\"pa"
                        }
                    }]
                },
                "finish_reason": null
            }]
        }"#;

        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        let tool_calls = chunk.choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(
            tool_calls[0]
                .function
                .as_ref()
                .unwrap()
                .arguments
                .as_deref(),
            Some("{\"pa")
        );
    }

    #[test]
    fn test_finish_reason() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        }"#;

        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.choices[0].finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn test_reasoning_content_delta() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1677652288,
            "model": "deepseek-chat",
            "choices": [{
                "index": 0,
                "delta": {
                    "reasoning_content": "Let me think..."
                },
                "finish_reason": null
            }]
        }"#;

        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        assert_eq!(
            chunk.choices[0].delta.reasoning_content.as_deref(),
            Some("Let me think...")
        );
    }

    #[test]
    fn test_usage_at_end() {
        let json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        }"#;

        let chunk: StreamChunk = serde_json::from_str(json).unwrap();
        let usage = chunk.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
    }
}
