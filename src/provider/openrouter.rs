use super::{
    ChatRequest, ContentBlock, Message, ModelInfo, ModelPricing, Provider, ProviderError,
    ProviderPrefs, Role, StreamEvent, ToolCallEvent, Usage,
};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest_eventsource::{Event, EventSource};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct OpenRouterProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    prefs: ProviderPrefs,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenRouterRequest {
    model: String,
    messages: Vec<OpenRouterMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenRouterToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenRouterToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenRouterFunctionDefinition,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenRouterFunctionDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct OpenRouterMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenRouterToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct OpenRouterToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenRouterFunctionCall,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct OpenRouterFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterChunk {
    choices: Vec<OpenRouterChunkChoice>,
    usage: Option<OpenRouterUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterChunkChoice {
    delta: OpenRouterDelta,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenRouterToolCallChunk>>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterToolCallChunk {
    index: Option<usize>,
    id: Option<String>,
    function: Option<OpenRouterFunctionCallChunk>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterFunctionCallChunk {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    #[allow(dead_code)]
    total_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModelsResponse {
    data: Vec<OpenRouterApiModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterApiModel {
    id: String,
    name: String,
    context_length: u32,
    #[serde(default)]
    pricing: OpenRouterPricing,
    #[serde(default)]
    created: u64,
}

#[derive(Debug, Deserialize, Default)]
struct OpenRouterPricing {
    #[serde(default)]
    prompt: serde_json::Value,
    #[serde(default)]
    completion: serde_json::Value,
}

impl OpenRouterPricing {
    /// Parse price value (can be string like "0.000003" or number or "-1" for special models)
    fn parse_price(value: &serde_json::Value) -> f64 {
        match value {
            serde_json::Value::String(s) => s.parse().unwrap_or(0.0),
            serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
            _ => 0.0,
        }
    }

    fn prompt_price(&self) -> f64 {
        Self::parse_price(&self.prompt)
    }

    fn completion_price(&self) -> f64 {
        Self::parse_price(&self.completion)
    }

    /// Check if this is a special routing model with variable pricing
    fn is_variable(&self) -> bool {
        self.prompt_price() < 0.0 || self.completion_price() < 0.0
    }
}

#[derive(Debug, Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl OpenRouterProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            prefs: ProviderPrefs::default(),
        }
    }

    pub fn with_prefs(api_key: String, prefs: ProviderPrefs) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            prefs,
        }
    }

    pub fn set_prefs(&mut self, prefs: ProviderPrefs) {
        self.prefs = prefs;
    }

    fn map_messages(messages: &[Message]) -> Vec<OpenRouterMessage> {
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
                        mapped.push(OpenRouterMessage {
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
                            calls.push(OpenRouterToolCall {
                                id: id.clone(),
                                call_type: "function".to_string(),
                                function: OpenRouterFunctionCall {
                                    name: name.clone(),
                                    arguments: arguments.to_string(),
                                },
                            });
                        }
                        ContentBlock::ToolResult { .. } => {}
                        ContentBlock::Image { .. } => {}
                    }
                }

                mapped.push(OpenRouterMessage {
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
impl Provider for OpenRouterProvider {
    fn id(&self) -> &str {
        "openrouter"
    }

    fn model_info(&self, _model_id: &str) -> Option<ModelInfo> {
        None
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![]
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        tracing::debug!("OpenRouter::list_models - fetching from {}/models", self.base_url);
        let response = self
            .client
            .get(format!("{}/models", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await?;

        tracing::debug!("OpenRouter::list_models - response status: {}", response.status());

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            tracing::debug!("OpenRouter::list_models - error: {}", text);
            return Err(ProviderError::Api {
                code: status.to_string(),
                message: text,
            });
        }

        let data: OpenRouterModelsResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Stream(format!("Failed to parse models: {}", e)))?;

        tracing::debug!("OpenRouter::list_models - parsed {} models", data.data.len());

        Ok(data
            .data
            .into_iter()
            .filter(|m| {
                // Filter out special routing models with variable pricing (-1)
                !m.pricing.is_variable()
            })
            .map(|m| {
                // Extract actual model provider from ID (e.g., "anthropic/claude-sonnet-4" -> "anthropic")
                let provider = m.id.split('/').next().unwrap_or("unknown").to_string();
                // Convert per-token to per-million-token pricing
                let input_price = m.pricing.prompt_price() * 1_000_000.0;
                let output_price = m.pricing.completion_price() * 1_000_000.0;
                let pricing = ModelPricing {
                    input: input_price,
                    output: output_price,
                    cache_read: None,
                    cache_write: None,
                };
                ModelInfo {
                    id: m.id,
                    name: m.name,
                    provider,
                    context_window: m.context_length,
                    supports_tools: true,
                    supports_vision: false,
                    supports_thinking: false,
                    supports_cache: false,
                    pricing,
                    created: m.created,
                }
            })
            .collect())
    }

    async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let or_request = OpenRouterRequest {
            model: request.model,
            messages: Self::map_messages(&request.messages),
            stream: Some(true),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools: request
                .tools
                .iter()
                .map(|t| OpenRouterToolDefinition {
                    tool_type: "function".to_string(),
                    function: OpenRouterFunctionDefinition {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    },
                })
                .collect(),
            include_reasoning: request.thinking.map(|t| t.enabled),
            provider: self.prefs.to_routing_params(),
        };

        let mut source = EventSource::new(
            self.client
                .post(format!("{}/chat/completions", self.base_url))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("HTTP-Referer", "https://github.com/nijaru/ion")
                .header("X-Title", "ion")
                .json(&or_request),
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

                    let chunk: OpenRouterChunk = match serde_json::from_str(&message.data) {
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
                        if let Some(reasoning) = choice.delta.reasoning {
                            let _ = tx.send(StreamEvent::ThinkingDelta(reasoning)).await;
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
        let or_request = OpenRouterRequest {
            model: request.model,
            messages: Self::map_messages(&request.messages),
            stream: Some(false),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools: request
                .tools
                .iter()
                .map(|t| OpenRouterToolDefinition {
                    tool_type: "function".to_string(),
                    function: OpenRouterFunctionDefinition {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    },
                })
                .collect(),
            include_reasoning: request.thinking.map(|t| t.enabled),
            provider: self.prefs.to_routing_params(),
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("HTTP-Referer", "https://github.com/nijaru/ion")
            .header("X-Title", "ion")
            .json(&or_request)
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
        let reasoning_str = message_val["reasoning_content"].as_str().unwrap_or("");

        let mut content_blocks = Vec::new();

        if !reasoning_str.is_empty() {
            content_blocks.push(ContentBlock::Thinking {
                thinking: reasoning_str.to_string(),
            });
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_map_messages_user_text() {
        let msgs = vec![Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: "hello".into(),
            }]),
        }];
        let mapped = OpenRouterProvider::map_messages(&msgs);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].role, "user");
        assert_eq!(mapped[0].content, Some("hello".into()));
    }

    #[test]
    fn test_map_messages_tool_call() {
        let msgs = vec![Message {
            role: Role::Assistant,
            content: Arc::new(vec![
                ContentBlock::Text {
                    text: "calling tool".into(),
                },
                ContentBlock::ToolCall {
                    id: "123".into(),
                    name: "test_tool".into(),
                    arguments: json!({"a": 1}),
                },
            ]),
        }];
        let mapped = OpenRouterProvider::map_messages(&msgs);
        assert_eq!(mapped[0].role, "assistant");
        assert_eq!(mapped[0].content, Some("calling tool".into()));
        assert_eq!(mapped[0].tool_calls.as_ref().unwrap()[0].id, "123");
        assert_eq!(
            mapped[0].tool_calls.as_ref().unwrap()[0].function.name,
            "test_tool"
        );
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
        let mapped = OpenRouterProvider::map_messages(&msgs);
        assert_eq!(mapped[0].role, "tool");
        assert_eq!(mapped[0].tool_call_id, Some("123".into()));
        assert_eq!(mapped[0].content, Some("result data".into()));
    }
}
