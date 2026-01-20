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

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicToolDefinition>,
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: AnthropicImageSource },
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
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicImageSource {
    #[serde(rename = "type")]
    source_type: String,
    media_type: String,
    data: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicToolDefinition {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: AnthropicMessageInfo },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: AnthropicContentBlockInfo,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: usize, delta: AnthropicDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { _index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta { usage: Option<AnthropicUsageDelta> },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: AnthropicError },
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageInfo {
    _id: String,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsageDelta {
    output_tokens: u32,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlockInfo {
    #[serde(rename = "text")]
    Text { _text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        _input: serde_json::Value,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Deserialize)]
struct AnthropicError {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

#[derive(Debug, Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://api.anthropic.com/v1".to_string(),
        }
    }

    fn map_messages(messages: &[Message]) -> Vec<AnthropicMessage> {
        let mut anthropic_messages = Vec::new();

        for m in messages {
            // Anthropic separates system prompt from messages
            if m.role == Role::System {
                continue;
            }

            let role = match m.role {
                Role::User | Role::ToolResult => "user",
                Role::Assistant => "assistant",
                Role::System => unreachable!(),
            }
            .to_string();

            let mut contents = Vec::new();

            for block in m.content.as_ref() {
                match block {
                    ContentBlock::Text { text } => {
                        contents.push(AnthropicContentBlock::Text { text: text.clone() });
                    }
                    ContentBlock::Thinking { thinking } => {
                        // Anthropic doesn't have a specific "thinking" block for input,
                        // so we wrap it in tags for context.
                        contents.push(AnthropicContentBlock::Text {
                            text: format!("<thought>\n{}\n</thought>\n", thinking),
                        });
                    }
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        contents.push(AnthropicContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: arguments.clone(),
                        });
                    }
                    ContentBlock::ToolResult {
                        tool_call_id,
                        content,
                        is_error,
                    } => {
                        contents.push(AnthropicContentBlock::ToolResult {
                            tool_use_id: tool_call_id.clone(),
                            content: content.clone(),
                            is_error: if *is_error { Some(true) } else { None },
                        });
                    }
                    ContentBlock::Image { media_type, data } => {
                        contents.push(AnthropicContentBlock::Image {
                            source: AnthropicImageSource {
                                source_type: "base64".to_string(),
                                media_type: media_type.clone(),
                                data: data.clone(),
                            },
                        });
                    }
                }
            }

            anthropic_messages.push(AnthropicMessage {
                role,
                content: contents,
            });
        }

        anthropic_messages
    }

    fn extract_system_prompt(messages: &[Message]) -> Option<String> {
        messages
            .iter()
            .find(|m| m.role == Role::System)
            .and_then(|m| {
                m.content.iter().find_map(|block| {
                    if let ContentBlock::Text { text } = block {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
            })
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
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
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
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
                if let (Some(id), Some(name)) = (m["id"].as_str(), m["display_name"].as_str()) {
                    models.push(ModelInfo {
                        id: id.to_string(),
                        name: name.to_string(),
                        provider: "anthropic".to_string(),
                        context_window: 0, // Hydrate later
                        supports_tools: true,
                        supports_vision: true,
                        supports_thinking: false,
                        supports_cache: false,
                        pricing: Default::default(),
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
        let ant_request = AnthropicRequest {
            model: request.model,
            messages: Self::map_messages(&request.messages),
            system: request
                .system
                .map(|s| s.into_owned())
                .or_else(|| Self::extract_system_prompt(&request.messages)),
            max_tokens: request.max_tokens.unwrap_or(4096),
            temperature: request.temperature,
            tools: request
                .tools
                .iter()
                .map(|t| AnthropicToolDefinition {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    input_schema: t.parameters.clone(),
                })
                .collect(),
            stream: true,
        };

        let mut source = EventSource::new(
            self.client
                .post(format!("{}/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&ant_request),
        )
        .map_err(|e| ProviderError::Stream(e.to_string()))?;

        let mut pending_tool_calls: Vec<PendingToolCall> = Vec::new();
        let mut total_input_tokens = 0;
        let mut _total_output_tokens = 0;

        while let Some(event) = source.next().await {
            match event {
                Ok(Event::Open) => continue,
                Ok(Event::Message(message)) => {
                    let ant_event: AnthropicEvent = match serde_json::from_str(&message.data) {
                        Ok(e) => e,
                        Err(e) => {
                            let _ = tx.send(StreamEvent::Error(e.to_string())).await;
                            continue;
                        }
                    };

                    match ant_event {
                        AnthropicEvent::MessageStart { message } => {
                            total_input_tokens = message.usage.input_tokens;
                            _total_output_tokens = message.usage.output_tokens;
                            let _ = tx
                                .send(StreamEvent::Usage(Usage {
                                    input_tokens: total_input_tokens,
                                    output_tokens: _total_output_tokens,
                                    ..Default::default()
                                }))
                                .await;
                        }
                        AnthropicEvent::ContentBlockStart {
                            index,
                            content_block,
                        } => {
                            if let AnthropicContentBlockInfo::ToolUse { id, name, .. } =
                                content_block
                            {
                                while pending_tool_calls.len() <= index {
                                    pending_tool_calls.push(PendingToolCall::default());
                                }
                                pending_tool_calls[index].id = id;
                                pending_tool_calls[index].name = name;
                            }
                        }
                        AnthropicEvent::ContentBlockDelta { index, delta } => match delta {
                            AnthropicDelta::TextDelta { text } => {
                                let _ = tx.send(StreamEvent::TextDelta(text)).await;
                            }
                            AnthropicDelta::InputJsonDelta { partial_json } => {
                                while pending_tool_calls.len() <= index {
                                    pending_tool_calls.push(PendingToolCall::default());
                                }
                                pending_tool_calls[index].arguments.push_str(&partial_json);
                            }
                        },
                        AnthropicEvent::MessageDelta { usage } => {
                            if let Some(u) = usage {
                                _total_output_tokens = u.output_tokens;
                                let _ = tx
                                    .send(StreamEvent::Usage(Usage {
                                        input_tokens: total_input_tokens,
                                        output_tokens: _total_output_tokens,
                                        ..Default::default()
                                    }))
                                    .await;
                            }
                        }
                        AnthropicEvent::Error { error } => {
                            let _ = tx.send(StreamEvent::Error(error.message.clone())).await;
                            return Err(ProviderError::Api {
                                code: "anthropic_error".into(),
                                message: error.message,
                            });
                        }
                        AnthropicEvent::MessageStop
                        | AnthropicEvent::ContentBlockStop { .. }
                        | AnthropicEvent::Ping => {}
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
        let ant_request = AnthropicRequest {
            model: request.model,
            messages: Self::map_messages(&request.messages),
            system: request
                .system
                .map(|s| s.into_owned())
                .or_else(|| Self::extract_system_prompt(&request.messages)),
            max_tokens: request.max_tokens.unwrap_or(4096),
            temperature: request.temperature,
            tools: request
                .tools
                .iter()
                .map(|t| AnthropicToolDefinition {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    input_schema: t.parameters.clone(),
                })
                .collect(),
            stream: false,
        };

        let response = self
            .client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&ant_request)
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

        let mut content_blocks = Vec::new();
        if let Some(content) = body["content"].as_array() {
            for block in content {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = block["text"].as_str() {
                            content_blocks.push(ContentBlock::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                    Some("tool_use") => {
                        if let (Some(id), Some(name), Some(input)) = (
                            block["id"].as_str(),
                            block["name"].as_str(),
                            block.get("input"),
                        ) {
                            content_blocks.push(ContentBlock::ToolCall {
                                id: id.to_string(),
                                name: name.to_string(),
                                arguments: input.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(Message {
            role: Role::Assistant,
            content: Arc::new(content_blocks),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_messages_user_text() {
        let msgs = vec![Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: "hello".into(),
            }]),
        }];
        let mapped = AnthropicProvider::map_messages(&msgs);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].role, "user");
        if let AnthropicContentBlock::Text { text } = &mapped[0].content[0] {
            assert_eq!(text, "hello");
        } else {
            panic!("Wrong content block type");
        }
    }

    #[test]
    fn test_map_messages_system_prompt() {
        let msgs = vec![
            Message {
                role: Role::System,
                content: Arc::new(vec![ContentBlock::Text {
                    text: "system prompt".into(),
                }]),
            },
            Message {
                role: Role::User,
                content: Arc::new(vec![ContentBlock::Text {
                    text: "hello".into(),
                }]),
            },
        ];
        let mapped = AnthropicProvider::map_messages(&msgs);
        assert_eq!(mapped.len(), 1); // System prompt excluded from messages
        assert_eq!(mapped[0].role, "user");

        let system = AnthropicProvider::extract_system_prompt(&msgs);
        assert_eq!(system, Some("system prompt".into()));
    }

    #[test]
    fn test_map_messages_tool_result() {
        let msgs = vec![Message {
            role: Role::ToolResult,
            content: Arc::new(vec![ContentBlock::ToolResult {
                tool_call_id: "123".into(),
                content: "result data".into(),
                is_error: false,
            }]),
        }];
        let mapped = AnthropicProvider::map_messages(&msgs);
        assert_eq!(mapped[0].role, "user");
        if let AnthropicContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } = &mapped[0].content[0]
        {
            assert_eq!(tool_use_id, "123");
            assert_eq!(content, "result data");
        } else {
            panic!("Wrong content block type");
        }
    }
}
