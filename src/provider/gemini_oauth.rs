//! Gemini OAuth client using Bearer authentication.
//!
//! The standard llm-connector Google provider uses API key auth (x-goog-api-key header).
//! OAuth tokens require Bearer auth (Authorization: Bearer header).
//! This module provides a minimal Gemini client for OAuth.

use crate::provider::error::Error;
use crate::provider::types::{ChatRequest, ContentBlock, Message, Role, StreamEvent};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Gemini OAuth client.
pub struct GeminiOAuthClient {
    client: reqwest::Client,
    access_token: String,
}

impl GeminiOAuthClient {
    /// Create a new Gemini OAuth client.
    pub fn new(access_token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            access_token: access_token.into(),
        }
    }

    /// Make a chat completion request.
    pub async fn complete(&self, request: ChatRequest) -> Result<Message, Error> {
        let url = format!("{BASE_URL}/models/{}:generateContent", request.model);

        let gemini_request = GeminiRequest::from_chat_request(&request);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .json(&gemini_request)
            .send()
            .await
            .map_err(|e| Error::Api(format!("Request failed: {e}")))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| Error::Api(format!("Failed to read response: {e}")))?;

        if !status.is_success() {
            return Err(Error::Api(format!(
                "Gemini API error: {} - {}",
                status, text
            )));
        }

        let gemini_response: GeminiResponse = serde_json::from_str(&text)
            .map_err(|e| Error::Api(format!("Failed to parse response: {e}\nBody: {text}")))?;

        Ok(gemini_response.into_message())
    }

    /// Stream a chat completion.
    pub async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        let url = format!(
            "{BASE_URL}/models/{}:streamGenerateContent?alt=sse",
            request.model
        );

        let gemini_request = GeminiRequest::from_chat_request(&request);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .json(&gemini_request)
            .send()
            .await
            .map_err(|e| Error::Stream(format!("Request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Stream(format!(
                "Gemini API error: {} - {}",
                status, text
            )));
        }

        // Parse SSE stream
        use futures::StreamExt;
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Error::Stream(format!("Stream error: {e}")))?;
            let text = String::from_utf8_lossy(&chunk);
            buffer.push_str(&text);

            // Process complete SSE events
            while let Some(pos) = buffer.find("\n\n") {
                let event = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                // Parse SSE event
                if let Some(data) = event.strip_prefix("data: ") {
                    if data.trim().is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<GeminiResponse>(data) {
                        Ok(response) => {
                            if let Some(text) = response.get_text() {
                                let _ = tx.send(StreamEvent::TextDelta(text)).await;
                            }
                            // TODO: Handle tool calls in streaming
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse Gemini SSE event: {e}");
                        }
                    }
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }
}

// --- Gemini API Types ---

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<GeminiFunctionResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    args: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiFunctionResponse {
    name: String,
    response: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiTool {
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Serialize)]
struct GeminiFunctionDeclaration {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    #[allow(dead_code)]
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    prompt_token_count: Option<u32>,
    candidates_token_count: Option<u32>,
    total_token_count: Option<u32>,
}

impl GeminiRequest {
    fn from_chat_request(request: &ChatRequest) -> Self {
        let mut contents = Vec::new();
        let mut system_instruction = None;

        for msg in request.messages.iter() {
            match msg.role {
                Role::System => {
                    // Gemini uses system_instruction field
                    let text = msg
                        .content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    if !text.is_empty() {
                        system_instruction = Some(GeminiContent {
                            role: None,
                            parts: vec![GeminiPart {
                                text: Some(text),
                                function_call: None,
                                function_response: None,
                            }],
                        });
                    }
                }
                Role::User => {
                    let parts: Vec<GeminiPart> = msg
                        .content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(GeminiPart {
                                    text: Some(text.clone()),
                                    function_call: None,
                                    function_response: None,
                                })
                            } else {
                                None
                            }
                        })
                        .collect();

                    if !parts.is_empty() {
                        contents.push(GeminiContent {
                            role: Some("user".to_string()),
                            parts,
                        });
                    }
                }
                Role::Assistant => {
                    let mut parts = Vec::new();

                    for block in msg.content.iter() {
                        match block {
                            ContentBlock::Text { text } => {
                                parts.push(GeminiPart {
                                    text: Some(text.clone()),
                                    function_call: None,
                                    function_response: None,
                                });
                            }
                            ContentBlock::ToolCall {
                                name, arguments, ..
                            } => {
                                parts.push(GeminiPart {
                                    text: None,
                                    function_call: Some(GeminiFunctionCall {
                                        name: name.clone(),
                                        args: arguments.clone(),
                                    }),
                                    function_response: None,
                                });
                            }
                            _ => {}
                        }
                    }

                    if !parts.is_empty() {
                        contents.push(GeminiContent {
                            role: Some("model".to_string()),
                            parts,
                        });
                    }
                }
                Role::ToolResult => {
                    for block in msg.content.iter() {
                        if let ContentBlock::ToolResult {
                            tool_call_id,
                            content,
                            ..
                        } = block
                        {
                            // Gemini expects function responses as user messages
                            contents.push(GeminiContent {
                                role: Some("user".to_string()),
                                parts: vec![GeminiPart {
                                    text: None,
                                    function_call: None,
                                    function_response: Some(GeminiFunctionResponse {
                                        name: tool_call_id.clone(), // Use tool_call_id as name
                                        response: serde_json::json!({ "result": content }),
                                    }),
                                }],
                            });
                        }
                    }
                }
            }
        }

        // Convert tools
        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(vec![GeminiTool {
                function_declarations: request
                    .tools
                    .iter()
                    .map(|t| GeminiFunctionDeclaration {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    })
                    .collect(),
            }])
        };

        Self {
            contents,
            system_instruction,
            tools,
            generation_config: None,
        }
    }
}

impl GeminiResponse {
    fn get_text(&self) -> Option<String> {
        self.candidates
            .as_ref()?
            .first()?
            .content
            .as_ref()?
            .parts
            .first()?
            .text
            .clone()
    }

    fn into_message(self) -> Message {
        let mut content_blocks = Vec::new();

        if let Some(content) = self
            .candidates
            .and_then(|mut c| c.pop())
            .and_then(|c| c.content)
        {
            for part in content.parts {
                if let Some(text) = part.text {
                    content_blocks.push(ContentBlock::Text { text });
                }
                if let Some(fc) = part.function_call {
                    content_blocks.push(ContentBlock::ToolCall {
                        id: format!("call_{}", fc.name), // Generate ID
                        name: fc.name,
                        arguments: fc.args,
                    });
                }
            }
        }

        Message {
            role: Role::Assistant,
            content: Arc::new(content_blocks),
        }
    }
}
