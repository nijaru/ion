//! Anthropic API request types.

use serde::Serialize;

/// Top-level request to the Anthropic Messages API.
#[derive(Debug, Serialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Vec<SystemBlock>>,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
}

/// System prompt block with optional cache control.
#[derive(Debug, Serialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl SystemBlock {
    /// Create a text system block.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            block_type: "text".to_string(),
            text: content.into(),
            cache_control: None,
        }
    }

    /// Add ephemeral cache control to this block.
    pub fn with_cache(mut self) -> Self {
        self.cache_control = Some(CacheControl::ephemeral());
        self
    }
}

/// Cache control configuration.
#[derive(Debug, Clone, Serialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String,
}

impl CacheControl {
    /// Create an ephemeral cache control marker.
    pub fn ephemeral() -> Self {
        Self {
            cache_type: "ephemeral".to_string(),
        }
    }
}

/// A message in the conversation.
#[derive(Debug, Serialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

/// Content block within a message.
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    #[serde(rename = "image")]
    Image { source: ImageSource },
}

/// Image source for vision requests.
#[derive(Debug, Serialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

/// Tool definition for the API.
#[derive(Debug, Serialize)]
pub struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

impl Default for AnthropicRequest {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_tokens: 8192,
            system: None,
            messages: Vec::new(),
            tools: None,
            temperature: None,
            stream: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_block_serialization() {
        let block = SystemBlock::text("You are helpful").with_cache();
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"cache_control\""));
        assert!(json.contains("\"type\":\"ephemeral\""));
    }

    #[test]
    fn test_content_block_serialization() {
        let block = ContentBlock::Text {
            text: "Hello".to_string(),
            cache_control: None,
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"Hello\""));
        assert!(!json.contains("cache_control"));
    }

    #[test]
    fn test_tool_result_serialization() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "call_123".to_string(),
            content: "result".to_string(),
            is_error: false,
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"tool_result\""));
        assert!(!json.contains("is_error")); // skipped when false
    }

    #[test]
    fn test_tool_result_error_serialization() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "call_123".to_string(),
            content: "error".to_string(),
            is_error: true,
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"is_error\":true"));
    }

    #[test]
    fn test_request_serialization() {
        let request = AnthropicRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 1024,
            system: Some(vec![SystemBlock::text("Be helpful").with_cache()]),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Hi".to_string(),
                    cache_control: None,
                }],
            }],
            tools: None,
            temperature: None,
            stream: true,
        };
        let json = serde_json::to_string_pretty(&request).unwrap();
        assert!(json.contains("claude-sonnet-4-20250514"));
        assert!(json.contains("cache_control"));
    }
}
