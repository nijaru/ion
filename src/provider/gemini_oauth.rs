//! Gemini OAuth client using Google Code Assist API.
//!
//! Uses `cloudcode-pa.googleapis.com` with OAuth authentication.
//! This is the same API used by Gemini CLI with "Login with Google".

use crate::provider::error::Error;
use crate::provider::types::{ChatRequest, ContentBlock, Message, Role, StreamEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Code Assist API base URL (internal Google API for Gemini CLI).
const CODE_ASSIST_API: &str = "https://cloudcode-pa.googleapis.com";

/// Strip models/ prefix if present (Code Assist API doesn't want it).
fn normalize_model_name(model: &str) -> &str {
    model.strip_prefix("models/").unwrap_or(model)
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

    /// Build headers for Code Assist API requests.
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
        headers.insert(
            reqwest::header::ACCEPT,
            "text/event-stream".parse().unwrap(),
        );
        // Headers required by Code Assist API (matching Gemini CLI)
        headers.insert(
            reqwest::header::USER_AGENT,
            "google-cloud-sdk vscode_cloudshelleditor/0.1".parse().unwrap(),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-goog-api-client"),
            "gl-node/22.17.0".parse().unwrap(),
        );
        // Client metadata required by Code Assist API
        headers.insert(
            reqwest::header::HeaderName::from_static("client-metadata"),
            r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#
                .parse()
                .unwrap(),
        );
        headers
    }

    /// Make a chat completion request.
    pub async fn complete(&self, request: ChatRequest) -> Result<Message, Error> {
        let model = normalize_model_name(&request.model);
        let gemini_request = CodeAssistRequest::from_chat_request(&request, model);

        // Code Assist API: /v1internal:generateContent
        let url = format!("{CODE_ASSIST_API}/v1internal:generateContent");

        let response = self
            .client
            .post(&url)
            .headers(self.build_headers())
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
            return Err(Error::Api(format!("Gemini API error: {status} - {text}")));
        }

        let ca_response: CodeAssistResponse = serde_json::from_str(&text)
            .map_err(|e| Error::Api(format!("Failed to parse response: {e}\nBody: {text}")))?;

        Ok(ca_response.response.into_message())
    }

    /// Stream a chat completion.
    pub async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        use futures::StreamExt;

        let model = normalize_model_name(&request.model);
        let gemini_request = CodeAssistRequest::from_chat_request(&request, model);

        // Code Assist API: /v1internal:streamGenerateContent
        let url = format!("{CODE_ASSIST_API}/v1internal:streamGenerateContent?alt=sse");

        // Debug: log the request
        if let Ok(json) = serde_json::to_string_pretty(&gemini_request) {
            tracing::debug!("Code Assist request: {}", json);
        }

        let response = self
            .client
            .post(&url)
            .headers(self.build_headers())
            .json(&gemini_request)
            .send()
            .await
            .map_err(|e| Error::Stream(format!("Request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Stream(format!("Gemini API error: {status} - {text}")));
        }

        // Parse SSE stream
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

                    match serde_json::from_str::<CodeAssistResponse>(data) {
                        Ok(ca_response) => {
                            if let Some(text) = ca_response.response.get_text() {
                                let _ = tx.send(StreamEvent::TextDelta(text)).await;
                            }
                            // TODO: Handle tool calls in streaming
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse Code Assist SSE event: {e}");
                        }
                    }
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }
}

// --- Code Assist API Types ---

/// Code Assist API request wrapper.
/// The API expects: { model, request: { contents, ... }, userAgent, requestId }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeAssistRequest {
    model: String,
    request: VertexRequest,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

/// Inner request structure for Code Assist API.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VertexRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

impl CodeAssistRequest {
    fn from_chat_request(request: &ChatRequest, model: &str) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let inner = GeminiRequest::from_chat_request(request);

        // Generate a unique request ID
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let random_suffix: u32 = timestamp as u32 ^ std::process::id();
        let request_id = format!("ion-{timestamp}-{random_suffix:x}");

        Self {
            model: model.to_string(),
            request: VertexRequest {
                contents: inner.contents,
                system_instruction: inner.system_instruction,
                tools: inner.tools,
                generation_config: inner.generation_config,
            },
            user_agent: Some("ion-coding-agent".to_string()),
            request_id: Some(request_id),
        }
    }
}

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

/// Code Assist API response wrapper.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodeAssistResponse {
    response: GeminiResponse,
    #[allow(dead_code)]
    trace_id: Option<String>,
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

#[allow(dead_code, clippy::struct_field_names)] // Field names match API response
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    prompt_token_count: Option<u32>,
    candidates_token_count: Option<u32>,
    total_token_count: Option<u32>,
}

impl GeminiRequest {
    #[allow(clippy::too_many_lines)]
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
