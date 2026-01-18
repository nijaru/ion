use super::{
    ChatRequest, ContentBlock, Message, ModelInfo, Provider, ProviderError, Role, StreamEvent,
    ToolCallEvent, Usage,
};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest_eventsource::{Event, EventSource};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct OpenAIProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIFunctionDefinition,
}

#[derive(Debug, Serialize, Deserialize)]
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
    #[allow(dead_code)]
    finish_reason: Option<String>,
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

impl OpenAIProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1".to_string(),
        }
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
                // OpenAI/OpenRouter expect one message per tool result
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

#[async_trait]
impl Provider for OpenAIProvider {
    fn id(&self) -> &str {
        "openai"
    }

    fn model_info(&self, _model_id: &str) -> Option<ModelInfo> {
        None
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![]
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let response = self
            .client
            .get(format!("{}/models", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
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

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Stream(format!("Failed to parse models: {}", e)))?;

        let mut models = Vec::new();
        if let Some(data) = body["data"].as_array() {
            for m in data {
                if let Some(id) = m["id"].as_str() {
                    models.push(ModelInfo {
                        id: id.to_string(),
                        name: id.to_string(),
                        provider: "openai".to_string(),
                        context_window: 0,
                        supports_tools: true,
                        supports_vision: false,
                        supports_thinking: false,
                        supports_cache: false,
                        pricing: Default::default(),
                        created: 0,
                    });
                }
            }
        }
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

        let mut source = EventSource::new(
            self.client
                .post(format!("{}/chat/completions", self.base_url))
                .header("Authorization", format!("Bearer {}", self.api_key))
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
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
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
