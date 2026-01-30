//! OpenAI-compatible API request types.

use super::quirks::ProviderQuirks;
use crate::provider::prefs::ProviderPrefs;
use serde::Serialize;

/// Top-level request to OpenAI-compatible APIs.
#[derive(Debug, Serialize)]
pub struct OpenAIRequest {
    pub model: String,
    pub messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAITool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderRouting>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
}

/// Provider routing configuration (OpenRouter specific).
#[derive(Debug, Clone, Serialize)]
pub struct ProviderRouting {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantizations: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
}

impl ProviderRouting {
    /// Create routing from provider preferences.
    pub fn from_prefs(prefs: &ProviderPrefs) -> Option<Self> {
        let has_content = prefs.order.is_some()
            || !prefs.allow_fallbacks
            || prefs.resolve_quantizations().is_some()
            || prefs.ignore.is_some()
            || prefs.only.is_some()
            || prefs.sort.is_some();

        if !has_content {
            return None;
        }

        let sort = prefs.sort.and_then(|s| match s {
            crate::provider::prefs::SortStrategy::Price => Some("price".to_string()),
            crate::provider::prefs::SortStrategy::Throughput => Some("throughput".to_string()),
            crate::provider::prefs::SortStrategy::Latency => Some("latency".to_string()),
            // Alphabetical and Newest are local-only
            _ => None,
        });

        Some(Self {
            order: prefs.order.clone(),
            allow_fallbacks: if prefs.allow_fallbacks {
                None
            } else {
                Some(false)
            },
            quantizations: prefs.resolve_quantizations(),
            ignore: prefs.ignore.clone(),
            only: prefs.only.clone(),
            sort,
        })
    }
}

/// A message in the conversation.
#[derive(Debug, Serialize)]
pub struct OpenAIMessage {
    pub role: String,
    pub content: MessageContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Message content - can be string or array of content parts.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

/// Content part for multimodal messages.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

/// Image URL for vision requests.
#[derive(Debug, Clone, Serialize)]
pub struct ImageUrl {
    pub url: String,
}

/// Tool call in an assistant message.
#[derive(Debug, Serialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

/// Function call details.
#[derive(Debug, Serialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Tool definition for the API.
#[derive(Debug, Serialize)]
pub struct OpenAITool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

/// Function definition within a tool.
#[derive(Debug, Serialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl OpenAIRequest {
    /// Apply provider quirks to the request.
    pub fn apply_quirks(mut self, quirks: &ProviderQuirks) -> Self {
        // Handle max_tokens vs max_completion_tokens
        if quirks.use_max_tokens {
            // Move max_completion_tokens to max_tokens
            if self.max_completion_tokens.is_some() && self.max_tokens.is_none() {
                self.max_tokens = self.max_completion_tokens.take();
            }
            self.max_completion_tokens = None;
        } else {
            // Move max_tokens to max_completion_tokens
            if self.max_tokens.is_some() && self.max_completion_tokens.is_none() {
                self.max_completion_tokens = self.max_tokens.take();
            }
            self.max_tokens = None;
        }

        // Skip store if provider doesn't support it
        if quirks.skip_store {
            self.store = None;
        }

        // Skip provider routing if not supported
        if !quirks.supports_provider_routing {
            self.provider = None;
        }

        self
    }
}

impl Default for OpenAIRequest {
    fn default() -> Self {
        Self {
            model: String::new(),
            messages: Vec::new(),
            tools: None,
            max_tokens: None,
            max_completion_tokens: None,
            temperature: None,
            store: None,
            provider: None,
            stream: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::api_provider::Provider;

    #[test]
    fn test_request_serialization() {
        let request = OpenAIRequest {
            model: "gpt-4".to_string(),
            messages: vec![OpenAIMessage {
                role: "user".to_string(),
                content: MessageContent::Text("Hello".to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            tools: None,
            max_tokens: Some(1024),
            max_completion_tokens: None,
            temperature: None,
            store: None,
            provider: None,
            stream: true,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("gpt-4"));
        assert!(json.contains("\"max_tokens\":1024"));
        assert!(!json.contains("max_completion_tokens"));
    }

    #[test]
    fn test_apply_quirks_groq() {
        let quirks = ProviderQuirks::for_provider(Provider::Groq);
        let request = OpenAIRequest {
            model: "llama-3.1-70b".to_string(),
            messages: vec![],
            max_completion_tokens: Some(1024),
            store: Some(false),
            ..Default::default()
        };

        let request = request.apply_quirks(&quirks);

        // Should convert to max_tokens
        assert_eq!(request.max_tokens, Some(1024));
        assert!(request.max_completion_tokens.is_none());
        // Should skip store
        assert!(request.store.is_none());
    }

    #[test]
    fn test_apply_quirks_openai() {
        let quirks = ProviderQuirks::for_provider(Provider::OpenAI);
        let request = OpenAIRequest {
            model: "gpt-4".to_string(),
            messages: vec![],
            max_tokens: Some(1024),
            store: Some(false),
            ..Default::default()
        };

        let request = request.apply_quirks(&quirks);

        // Should convert to max_completion_tokens
        assert!(request.max_tokens.is_none());
        assert_eq!(request.max_completion_tokens, Some(1024));
        // Should keep store
        assert_eq!(request.store, Some(false));
    }

    #[test]
    fn test_multimodal_message() {
        let message = OpenAIMessage {
            role: "user".to_string(),
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "What's in this image?".to_string(),
                },
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "data:image/png;base64,abc123".to_string(),
                    },
                },
            ]),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };

        let json = serde_json::to_string(&message).unwrap();
        assert!(json.contains("image_url"));
        assert!(json.contains("base64"));
    }

    #[test]
    fn test_tool_call_message() {
        let message = OpenAIMessage {
            role: "assistant".to_string(),
            content: MessageContent::Text(String::new()),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_123".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: r#"{"path": "/etc/hosts"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
        };

        let json = serde_json::to_string(&message).unwrap();
        assert!(json.contains("call_123"));
        assert!(json.contains("read_file"));
    }

    #[test]
    fn test_provider_routing() {
        let prefs = ProviderPrefs {
            order: Some(vec!["Anthropic".to_string(), "OpenAI".to_string()]),
            allow_fallbacks: false,
            ..Default::default()
        };

        let routing = ProviderRouting::from_prefs(&prefs).unwrap();
        assert_eq!(
            routing.order,
            Some(vec!["Anthropic".to_string(), "OpenAI".to_string()])
        );
        assert_eq!(routing.allow_fallbacks, Some(false));
    }
}
