//! Anthropic streaming event types.

use super::response::Usage;
use serde::Deserialize;

/// Streaming event from the Anthropic Messages API.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
#[allow(dead_code)]
pub enum StreamEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: MessageStart },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: ContentBlockInfo,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: usize, delta: ContentDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta { delta: MessageDelta, usage: Usage },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: ApiError },
}

/// Initial message info.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct MessageStart {
    pub id: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub role: String,
    pub model: String,
    pub usage: Usage,
}

/// Content block type info at start.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
#[allow(dead_code)]
pub enum ContentBlockInfo {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
}

/// Delta update for a content block.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ContentDelta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
}

/// Final message delta with stop info.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct MessageDelta {
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
}

/// API error in stream.
#[derive(Debug, Deserialize)]
pub struct ApiError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_start_event() {
        let json = r#"{
            "type": "message_start",
            "message": {
                "id": "msg_123",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-20250514",
                "usage": {"input_tokens": 10, "output_tokens": 0}
            }
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        if let StreamEvent::MessageStart { message } = event {
            assert_eq!(message.id, "msg_123");
        } else {
            panic!("Expected MessageStart");
        }
    }

    #[test]
    fn test_content_block_start_text() {
        let json = r#"{
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        if let StreamEvent::ContentBlockStart { index, .. } = event {
            assert_eq!(index, 0);
        } else {
            panic!("Expected ContentBlockStart");
        }
    }

    #[test]
    fn test_content_block_start_tool_use() {
        let json = r#"{
            "type": "content_block_start",
            "index": 1,
            "content_block": {"type": "tool_use", "id": "call_456", "name": "bash"}
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        if let StreamEvent::ContentBlockStart {
            index,
            content_block,
        } = event
        {
            assert_eq!(index, 1);
            if let ContentBlockInfo::ToolUse { id, name } = content_block {
                assert_eq!(id, "call_456");
                assert_eq!(name, "bash");
            }
        } else {
            panic!("Expected ContentBlockStart with tool_use");
        }
    }

    #[test]
    fn test_text_delta() {
        let json = r#"{
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hello"}
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        if let StreamEvent::ContentBlockDelta { delta, .. } = event {
            if let ContentDelta::Text { text } = delta {
                assert_eq!(text, "Hello");
            } else {
                panic!("Expected TextDelta");
            }
        } else {
            panic!("Expected ContentBlockDelta");
        }
    }

    #[test]
    fn test_thinking_delta() {
        let json = r#"{
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "Let me consider..."}
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        if let StreamEvent::ContentBlockDelta { delta, .. } = event {
            if let ContentDelta::Thinking { thinking } = delta {
                assert!(thinking.contains("consider"));
            } else {
                panic!("Expected ThinkingDelta");
            }
        } else {
            panic!("Expected ContentBlockDelta");
        }
    }

    #[test]
    fn test_tool_input_delta() {
        let json = r#"{
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        if let StreamEvent::ContentBlockDelta { delta, .. } = event {
            if let ContentDelta::InputJson { partial_json } = delta {
                assert!(partial_json.contains("path"));
            } else {
                panic!("Expected InputJsonDelta");
            }
        } else {
            panic!("Expected ContentBlockDelta");
        }
    }

    #[test]
    fn test_message_delta() {
        let json = r#"{
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"input_tokens": 10, "output_tokens": 20}
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        if let StreamEvent::MessageDelta { delta, usage } = event {
            assert_eq!(delta.stop_reason.as_deref(), Some("end_turn"));
            assert_eq!(usage.output_tokens, 20);
        } else {
            panic!("Expected MessageDelta");
        }
    }

    #[test]
    fn test_message_delta_output_tokens_only() {
        // Anthropic's message_delta often sends only output_tokens
        let json = r#"{
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 42}
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        if let StreamEvent::MessageDelta { usage, .. } = event {
            assert_eq!(usage.input_tokens, 0);
            assert_eq!(usage.output_tokens, 42);
        } else {
            panic!("Expected MessageDelta");
        }
    }

    #[test]
    fn test_error_event() {
        let json = r#"{
            "type": "error",
            "error": {"type": "overloaded_error", "message": "API overloaded"}
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        if let StreamEvent::Error { error } = event {
            assert_eq!(error.error_type, "overloaded_error");
        } else {
            panic!("Expected Error");
        }
    }

    #[test]
    fn test_ping_event() {
        let json = r#"{"type": "ping"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, StreamEvent::Ping));
    }
}
