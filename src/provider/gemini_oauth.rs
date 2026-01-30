//! Gemini OAuth client using Antigravity (Code Assist API).
//!
//! The consumer Gemini API (generativelanguage.googleapis.com) only supports API keys.
//! OAuth access requires the Code Assist API (cloudcode-pa.googleapis.com).
//! This module provides a Gemini client for OAuth via Antigravity.

use crate::provider::error::Error;
use crate::provider::types::{ChatRequest, ContentBlock, Message, Role, StreamEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Code Assist API endpoints (Antigravity).
const CODE_ASSIST_ENDPOINTS: &[&str] = &[
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
    "https://autopush-cloudcode-pa.sandbox.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];

/// API version for Code Assist.
const API_VERSION: &str = "v1internal";

/// User agent for Antigravity requests.
const USER_AGENT: &str = "antigravity/1.15.8 darwin/arm64";

/// API client identifier.
const API_CLIENT: &str = "google-cloud-sdk vscode_cloudshelleditor/0.1";

/// Map model names to Code Assist API names.
fn map_model_name(model: &str) -> &str {
    match model {
        "gemini-3-flash-preview" => "gemini-3-flash",
        "gemini-3-pro-preview" => "gemini-3-pro-high",
        "gemini-3-pro-image-preview" => "gemini-3-pro-image",
        "gemini-2.5-flash" => "gemini-2.5-flash",
        "gemini-2.5-pro" => "gemini-2.5-pro",
        other => other,
    }
}

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

    /// Build headers for Antigravity requests.
    fn build_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.access_token).parse().unwrap(),
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        headers.insert(reqwest::header::USER_AGENT, USER_AGENT.parse().unwrap());
        headers.insert("X-Goog-Api-Client", API_CLIENT.parse().unwrap());
        headers
    }

    /// Try request with endpoint fallback.
    async fn request_with_fallback(
        &self,
        action: &str,
        body: &impl Serialize,
    ) -> Result<reqwest::Response, Error> {
        let headers = self.build_headers();

        for (i, endpoint) in CODE_ASSIST_ENDPOINTS.iter().enumerate() {
            // Code Assist API uses /{version}:{action} format, not /models/{model}:{action}
            let url = format!("{endpoint}/{API_VERSION}:{action}");

            let response = self
                .client
                .post(&url)
                .headers(headers.clone())
                .json(body)
                .send()
                .await;

            match response {
                Ok(resp) if resp.status().is_success() || i == CODE_ASSIST_ENDPOINTS.len() - 1 => {
                    return Ok(resp);
                }
                Ok(_) => continue, // Try next endpoint
                Err(e) if i == CODE_ASSIST_ENDPOINTS.len() - 1 => {
                    return Err(Error::Api(format!("Request failed: {e}")));
                }
                Err(_) => continue, // Try next endpoint
            }
        }

        Err(Error::Api("All endpoints failed".to_string()))
    }

    /// Make a chat completion request.
    pub async fn complete(&self, request: ChatRequest) -> Result<Message, Error> {
        let model = map_model_name(&request.model);
        let gemini_request = GeminiRequest::from_chat_request(&request);
        let wrapped = CodeAssistRequest::new(model, &gemini_request);

        let response = self
            .request_with_fallback("generateContent", &wrapped)
            .await?;

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
        let headers = self.build_headers();
        let model = map_model_name(&request.model);
        let gemini_request = GeminiRequest::from_chat_request(&request);
        let wrapped = CodeAssistRequest::new(model, &gemini_request);

        // Try each endpoint until one succeeds
        let mut last_error = None;
        let mut response = None;

        for endpoint in CODE_ASSIST_ENDPOINTS {
            // Code Assist API uses /{version}:{action} format
            let url = format!(
                "{endpoint}/{API_VERSION}:streamGenerateContent?alt=sse"
            );

            match self
                .client
                .post(&url)
                .headers(headers.clone())
                .json(&wrapped)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    response = Some(resp);
                    break;
                }
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    last_error = Some(format!("Gemini API error: {status} - {text}"));
                }
                Err(e) => {
                    last_error = Some(format!("Request failed: {e}"));
                }
            }
        }

        let response = response.ok_or_else(|| {
            Error::Stream(last_error.unwrap_or_else(|| "All endpoints failed".to_string()))
        })?;

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

// --- Code Assist API Wrapper ---

/// Wrapper for Code Assist API requests.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeAssistRequest<'a> {
    /// Model name (mapped for Code Assist)
    model: &'a str,
    /// The inner Gemini request
    request: &'a GeminiRequest,
    /// User agent identifier
    user_agent: &'static str,
}

impl<'a> CodeAssistRequest<'a> {
    fn new(model: &'a str, request: &'a GeminiRequest) -> Self {
        Self {
            model,
            request,
            user_agent: "antigravity",
        }
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

        // Build a map of tool_call_id -> function_name from assistant messages
        let mut tool_call_names: HashMap<String, String> = HashMap::new();
        for msg in request.messages.iter() {
            if msg.role == Role::Assistant {
                for block in msg.content.iter() {
                    if let ContentBlock::ToolCall { id, name, .. } = block {
                        tool_call_names.insert(id.clone(), name.clone());
                    }
                }
            }
        }

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
                            // Gemini expects function name, not tool_call_id
                            // Look up the function name from our map
                            let function_name = tool_call_names
                                .get(tool_call_id)
                                .cloned()
                                .unwrap_or_else(|| tool_call_id.clone());

                            // Gemini expects function responses as user messages
                            contents.push(GeminiContent {
                                role: Some("user".to_string()),
                                parts: vec![GeminiPart {
                                    text: None,
                                    function_call: None,
                                    function_response: Some(GeminiFunctionResponse {
                                        name: function_name,
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
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        // Generate unique IDs using timestamp + counter
        static COUNTER: AtomicU64 = AtomicU64::new(0);

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
                    // Generate unique ID: timestamp_counter_name
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
                    let id = format!("call_{}_{ts}_{count}", fc.name);

                    content_blocks.push(ContentBlock::ToolCall {
                        id,
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
