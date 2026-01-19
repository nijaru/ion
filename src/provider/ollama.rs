//! Ollama provider for local LLM inference.
//!
//! Auto-discovers models at localhost:11434 (or OLLAMA_HOST).
//! Uses OpenAI-compatible API for chat completions.

use super::{
    ChatRequest, ContentBlock, Message, ModelInfo, Provider, ProviderError, Role, StreamEvent,
    ToolCallEvent, Usage,
};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest_eventsource::{Event, EventSource};
use serde::{Deserialize, Serialize};
use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;

const DEFAULT_HOST: &str = "http://localhost:11434";

pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
}

// OpenAI-compatible request/response structures (reused from openai.rs)
#[derive(Debug, Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAIToolDefinition>,
}

#[derive(Debug, Serialize)]
struct OpenAIToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIFunctionDefinition,
}

#[derive(Debug, Serialize)]
struct OpenAIFunctionDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct OpenAIMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAIToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenAIFunctionCall,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct OpenAIFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIChunk {
    choices: Vec<OpenAIChunkChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChunkChoice {
    delta: OpenAIDelta,
}

#[derive(Debug, Deserialize)]
struct OpenAIDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAIToolCallChunk>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIToolCallChunk {
    index: Option<usize>,
    id: Option<String>,
    function: Option<OpenAIFunctionCallChunk>,
}

#[derive(Debug, Deserialize)]
struct OpenAIFunctionCallChunk {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

#[derive(Debug, Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

// Ollama-native API response for listing models
#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaModel {
    name: String,
    #[serde(default)]
    details: OllamaModelDetails,
}

#[derive(Debug, Deserialize, Default)]
struct OllamaModelDetails {
    #[serde(default)]
    parameter_size: String,
    #[serde(default)]
    family: String,
}

impl OllamaProvider {
    pub fn new() -> Self {
        let base_url = env::var("OLLAMA_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_string());
        Self {
            client: reqwest::Client::new(),
            base_url,
        }
    }

    /// Check if Ollama is reachable at the configured host.
    pub async fn is_available(&self) -> bool {
        self.client
            .get(format!("{}/api/tags", self.base_url))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    fn map_messages(messages: &[Message]) -> Vec<OpenAIMessage> {
        let mut mapped = Vec::new();

        for m in messages {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::ToolResult => "tool",
            }
            .to_string();

            if m.role == Role::ToolResult {
                for block in m.content.as_ref() {
                    if let ContentBlock::ToolResult {
                        tool_call_id: id,
                        content: res,
                        is_error: _,
                    } = block
                    {
                        mapped.push(OpenAIMessage {
                            role: role.clone(),
                            content: Some(res.clone()),
                            tool_calls: None,
                            tool_call_id: Some(id.clone()),
                        });
                    }
                }
            } else {
                let mut content = String::new();
                let mut tool_calls = None;

                for block in m.content.as_ref() {
                    match block {
                        ContentBlock::Text { text } => content.push_str(text),
                        ContentBlock::Thinking { thinking } => {
                            content.push_str(&format!("<thought>\n{}\n</thought>\n", thinking))
                        }
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        } => {
                            let calls = tool_calls.get_or_insert_with(Vec::new);
                            calls.push(OpenAIToolCall {
                                id: id.clone(),
                                call_type: "function".to_string(),
                                function: OpenAIFunctionCall {
                                    name: name.clone(),
                                    arguments: arguments.to_string(),
                                },
                            });
                        }
                        ContentBlock::ToolResult { .. } => {}
                        ContentBlock::Image { .. } => {}
                    }
                }

                mapped.push(OpenAIMessage {
                    role,
                    content: if content.is_empty() {
                        None
                    } else {
                        Some(content)
                    },
                    tool_calls,
                    tool_call_id: None,
                });
            }
        }
        mapped
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    fn id(&self) -> &str {
        "ollama"
    }

    fn model_info(&self, _model_id: &str) -> Option<ModelInfo> {
        None
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![]
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        // Use Ollama's native API to list models
        let response = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                code: status.to_string(),
                message: text,
            });
        }

        let tags: OllamaTagsResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Stream(format!("Failed to parse models: {}", e)))?;

        let models = tags
            .models
            .into_iter()
            .map(|m| {
                // Extract base name without tag for display
                let name = m.name.split(':').next().unwrap_or(&m.name).to_string();
                let context_window = if m.details.parameter_size.contains("70") {
                    128_000
                } else if m.details.parameter_size.contains("32")
                    || m.details.parameter_size.contains("34")
                {
                    128_000
                } else {
                    32_000
                };

                ModelInfo {
                    id: m.name.clone(),
                    name,
                    provider: "ollama".to_string(),
                    context_window,
                    supports_tools: true, // Most recent models support tools
                    supports_vision: m.details.family.contains("llava")
                        || m.name.contains("vision"),
                    supports_thinking: false,
                    supports_cache: false,
                    pricing: Default::default(), // Local = free
                    created: 0,
                }
            })
            .collect();

        Ok(models)
    }

    async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let oa_request = OpenAIRequest {
            model: request.model,
            messages: Self::map_messages(&request.messages),
            stream: Some(true),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools: request
                .tools
                .iter()
                .map(|t| OpenAIToolDefinition {
                    tool_type: "function".to_string(),
                    function: OpenAIFunctionDefinition {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    },
                })
                .collect(),
        };

        // Ollama uses OpenAI-compatible endpoint at /v1/chat/completions
        let mut source = EventSource::new(
            self.client
                .post(format!("{}/v1/chat/completions", self.base_url))
                .json(&oa_request),
        )
        .map_err(|e| ProviderError::Stream(e.to_string()))?;

        let mut pending_tool_calls: Vec<PendingToolCall> = Vec::new();

        while let Some(event) = source.next().await {
            match event {
                Ok(Event::Open) => continue,
                Ok(Event::Message(message)) => {
                    if message.data == "[DONE]" {
                        break;
                    }

                    let chunk: OpenAIChunk = match serde_json::from_str(&message.data) {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = tx.send(StreamEvent::Error(e.to_string())).await;
                            continue;
                        }
                    };

                    for choice in chunk.choices {
                        if let Some(content) = choice.delta.content {
                            let _ = tx.send(StreamEvent::TextDelta(content)).await;
                        }
                        if let Some(tool_calls) = choice.delta.tool_calls {
                            for tc in tool_calls {
                                let index = tc.index.unwrap_or(0);
                                while pending_tool_calls.len() <= index {
                                    pending_tool_calls.push(PendingToolCall::default());
                                }
                                let pending = &mut pending_tool_calls[index];

                                if let Some(id) = tc.id {
                                    pending.id = id;
                                }
                                if let Some(func) = tc.function {
                                    if let Some(name) = func.name {
                                        pending.name.push_str(&name);
                                    }
                                    if let Some(args) = func.arguments {
                                        pending.arguments.push_str(&args);
                                    }
                                }
                            }
                        }
                    }

                    if let Some(usage) = chunk.usage {
                        let _ = tx
                            .send(StreamEvent::Usage(Usage {
                                input_tokens: usage.prompt_tokens,
                                output_tokens: usage.completion_tokens,
                                ..Default::default()
                            }))
                            .await;
                    }
                }
                Err(e) => {
                    let _ = tx.send(StreamEvent::Error(e.to_string())).await;
                    return Err(ProviderError::Stream(e.to_string()));
                }
            }
        }

        // Send accumulated tool calls
        for pending in pending_tool_calls {
            if !pending.id.is_empty() && !pending.name.is_empty() {
                let _ = tx
                    .send(StreamEvent::ToolCall(ToolCallEvent {
                        id: pending.id,
                        name: pending.name,
                        arguments: serde_json::from_str(&pending.arguments)
                            .unwrap_or(serde_json::Value::Null),
                    }))
                    .await;
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }

    async fn complete(&self, request: ChatRequest) -> Result<Message, ProviderError> {
        let oa_request = OpenAIRequest {
            model: request.model,
            messages: Self::map_messages(&request.messages),
            stream: Some(false),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools: request
                .tools
                .iter()
                .map(|t| OpenAIToolDefinition {
                    tool_type: "function".to_string(),
                    function: OpenAIFunctionDefinition {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    },
                })
                .collect(),
        };

        let response = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&oa_request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await?;
            return Err(ProviderError::Api {
                code: status.to_string(),
                message: text,
            });
        }

        let body: serde_json::Value = response.json().await?;

        let choice = &body["choices"][0];
        let message_val = &choice["message"];
        let content_str = message_val["content"].as_str().unwrap_or("");

        let mut content_blocks = Vec::new();

        if !content_str.is_empty() {
            content_blocks.push(ContentBlock::Text {
                text: content_str.to_string(),
            });
        }

        if let Some(tool_calls) = message_val["tool_calls"].as_array() {
            for tc in tool_calls {
                if let (Some(id), Some(name), Some(args)) = (
                    tc["id"].as_str(),
                    tc["function"]["name"].as_str(),
                    tc["function"]["arguments"].as_str(),
                ) {
                    content_blocks.push(ContentBlock::ToolCall {
                        id: id.to_string(),
                        name: name.to_string(),
                        arguments: serde_json::from_str(args).unwrap_or(serde_json::Value::Null),
                    });
                }
            }
        }

        Ok(Message {
            role: Role::Assistant,
            content: Arc::new(content_blocks),
        })
    }
}
