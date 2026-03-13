//! Tests for OpenAI-compatible client.

#![cfg(test)]

use super::client::OpenAICompatClient;
use crate::provider::api_provider::Provider;
use crate::provider::prefs::ProviderPrefs;
use crate::provider::types::{ChatRequest, ContentBlock, Message, Role, ToolDefinition};
use std::sync::Arc;

#[test]
fn test_build_request_basic() {
    let client = OpenAICompatClient::new(Provider::OpenAI, "test-key").unwrap();

    let request = ChatRequest {
        model: "gpt-4".to_string(),
        messages: Arc::new(vec![Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: "Hello".to_string(),
            }]),
        }]),
        system: None,
        tools: Arc::new(vec![]),
        max_tokens: Some(1024),
        temperature: None,
        thinking: None,
    };

    let api_request = client.build_request_for_test(&request, None, false);

    assert_eq!(api_request.model, "gpt-4");
    assert_eq!(api_request.messages.len(), 1);
    // OpenAI uses max_completion_tokens
    assert!(api_request.max_tokens.is_none());
    assert_eq!(api_request.max_completion_tokens, Some(1024));
}

#[test]
fn test_build_request_with_system() {
    let client = OpenAICompatClient::new(Provider::OpenAI, "test-key").unwrap();

    let request = ChatRequest {
        model: "gpt-4".to_string(),
        messages: Arc::new(vec![
            Message {
                role: Role::System,
                content: Arc::new(vec![ContentBlock::Text {
                    text: "You are helpful".to_string(),
                }]),
            },
            Message {
                role: Role::User,
                content: Arc::new(vec![ContentBlock::Text {
                    text: "Hi".to_string(),
                }]),
            },
        ]),
        system: None,
        tools: Arc::new(vec![]),
        max_tokens: None,
        temperature: None,
        thinking: None,
    };

    let api_request = client.build_request_for_test(&request, None, false);

    // Should have developer role for OpenAI
    assert_eq!(api_request.messages[0].role, "developer");
}

#[test]
fn test_build_request_groq_system_role() {
    let client = OpenAICompatClient::new(Provider::Groq, "test-key").unwrap();

    let request = ChatRequest {
        model: "llama-3.1-70b".to_string(),
        messages: Arc::new(vec![Message {
            role: Role::System,
            content: Arc::new(vec![ContentBlock::Text {
                text: "You are helpful".to_string(),
            }]),
        }]),
        system: None,
        tools: Arc::new(vec![]),
        max_tokens: Some(1024),
        temperature: None,
        thinking: None,
    };

    let api_request = client.build_request_for_test(&request, None, false);

    // Groq should use system role, not developer
    assert_eq!(api_request.messages[0].role, "system");
    // Groq uses max_tokens
    assert_eq!(api_request.max_tokens, Some(1024));
    assert!(api_request.max_completion_tokens.is_none());
}

#[test]
fn test_build_request_with_tools() {
    let client = OpenAICompatClient::new(Provider::OpenAI, "test-key").unwrap();

    let request = ChatRequest {
        model: "gpt-4".to_string(),
        messages: Arc::new(vec![Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: "Read /etc/hosts".to_string(),
            }]),
        }]),
        system: None,
        tools: Arc::new(vec![ToolDefinition {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                }
            }),
        }]),
        max_tokens: None,
        temperature: None,
        thinking: None,
    };

    let api_request =
        client.build_request_for_test(&request, Some(&ProviderPrefs::default()), true);

    assert!(api_request.tools.is_some());
    assert_eq!(api_request.tools.as_ref().unwrap().len(), 1);
    assert!(api_request.stream);
}

#[test]
fn test_build_request_with_tool_result() {
    let client = OpenAICompatClient::new(Provider::OpenAI, "test-key").unwrap();

    let request = ChatRequest {
        model: "gpt-4".to_string(),
        messages: Arc::new(vec![
            Message {
                role: Role::Assistant,
                content: Arc::new(vec![ContentBlock::ToolCall {
                    id: "call_123".to_string(),
                    name: "bash".to_string(),
                    arguments: serde_json::json!({"command": "ls"}),
                }]),
            },
            Message {
                role: Role::ToolResult,
                content: Arc::new(vec![ContentBlock::ToolResult {
                    tool_call_id: "call_123".to_string(),
                    content: "file1.txt".to_string(),
                    is_error: false,
                }]),
            },
        ]),
        system: None,
        tools: Arc::new(vec![]),
        max_tokens: None,
        temperature: None,
        thinking: None,
    };

    let api_request = client.build_request_for_test(&request, None, false);

    // Should have assistant message with tool_calls, then tool message
    assert_eq!(api_request.messages.len(), 2);
    assert_eq!(api_request.messages[1].role, "tool");
    assert_eq!(
        api_request.messages[1].tool_call_id.as_deref(),
        Some("call_123")
    );
}

#[test]
fn test_openrouter_provider_routing() {
    let client = OpenAICompatClient::new(Provider::OpenRouter, "test-key").unwrap();

    let prefs = ProviderPrefs {
        order: Some(vec!["Anthropic".to_string()]),
        allow_fallbacks: false,
        ..Default::default()
    };

    let request = ChatRequest {
        model: "anthropic/claude-sonnet-4-20250514".to_string(),
        messages: Arc::new(vec![Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: "Hi".to_string(),
            }]),
        }]),
        system: None,
        tools: Arc::new(vec![]),
        max_tokens: None,
        temperature: None,
        thinking: None,
    };

    let api_request = client.build_request_for_test(&request, Some(&prefs), false);

    assert!(api_request.provider.is_some());
    let routing = api_request.provider.unwrap();
    assert_eq!(routing.order, Some(vec!["Anthropic".to_string()]));
    assert_eq!(routing.allow_fallbacks, Some(false));
}
