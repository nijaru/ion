//! Gemini OAuth client using Code Assist API.
//!
//! Uses `cloudcode-pa.googleapis.com` with OAuth authentication.
//! This is the same backend used by Gemini CLI for consumer subscriptions.

use super::types::CodeAssistResponse;
use crate::provider::error::Error;
use crate::provider::http::{AuthConfig, HttpClient, SseParser};
use crate::provider::types::{ChatRequest, CompletionResponse, StreamEvent, Usage};
use futures::StreamExt;
use tokio::sync::mpsc;

/// Code Assist API endpoint (matching Gemini CLI).
const CODE_ASSIST_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com";
const CODE_ASSIST_API_VERSION: &str = "v1internal";

const DEFAULT_PROJECT_ID: &str = "rising-fact-p41fc";

/// Normalize model name (strip any prefix, no models/ prefix added).
fn normalize_model_name(model: &str) -> String {
    let trimmed = model.trim();
    trimmed
        .strip_prefix("models/")
        .unwrap_or(trimmed)
        .to_string()
}

/// Gemini OAuth client.
pub struct GeminiOAuthClient {
    http: HttpClient,
    project_id: String,
}

impl GeminiOAuthClient {
    /// Create a new Gemini OAuth client.
    pub fn new(access_token: impl Into<String>, project_id: Option<String>) -> Self {
        let access_token = access_token.into();
        let project_id = project_id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_PROJECT_ID.to_string());

        let http = HttpClient::new(CODE_ASSIST_ENDPOINT, AuthConfig::Bearer(access_token));

        Self { http, project_id }
    }

    /// Make a chat completion request.
    pub async fn complete(&self, request: ChatRequest) -> Result<CompletionResponse, Error> {
        let model = normalize_model_name(&request.model);
        let gemini_request =
            super::types::CodeAssistRequest::from_chat_request(&request, &model, &self.project_id);

        let url = format!("/{CODE_ASSIST_API_VERSION}:generateContent");

        let response: CodeAssistResponse = self.http.post_json(&url, &gemini_request).await?;

        Ok(CompletionResponse {
            message: response.response.into_message(),
            usage: Usage::default(),
        })
    }

    /// Stream a chat completion.
    pub async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        let model = normalize_model_name(&request.model);
        let gemini_request =
            super::types::CodeAssistRequest::from_chat_request(&request, &model, &self.project_id);

        if let Ok(json) = serde_json::to_string_pretty(&gemini_request) {
            tracing::debug!("Code Assist request: {}", json);
        }

        let url = format!("/{CODE_ASSIST_API_VERSION}:streamGenerateContent?alt=sse");

        let stream = self.http.post_stream(&url, &gemini_request).await?;
        futures::pin_mut!(stream);

        let mut parser = SseParser::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| Error::Stream(format!("Stream error: {e}")))?;
            let text = String::from_utf8_lossy(&chunk);

            for event in parser.feed(&text) {
                if event.data.is_empty() || event.data == "[DONE]" {
                    continue;
                }

                match serde_json::from_str::<CodeAssistResponse>(&event.data) {
                    Ok(ca_response) => {
                        if let Some(text) = ca_response.response.get_text() {
                            let _ = tx.send(StreamEvent::TextDelta(text)).await;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse Code Assist SSE event: {e}");
                    }
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }
}
