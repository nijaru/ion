//! ChatGPT subscription client using Responses API.

use super::convert::{build_request, extract_output_text, parse_response_event};
use super::types::ParsedEvent;
use crate::provider::error::Error;
use crate::provider::http::{AuthConfig, HttpClient, SseParser};
use crate::provider::types::{
    ChatRequest, CompletionResponse, ContentBlock, Message, Role, StreamEvent, Usage,
};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::sync::Arc;
use tokio::sync::mpsc;

/// ChatGPT Codex backend base URL (matches Codex CLI).
const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
/// Originator header value (match Codex CLI).
const ORIGINATOR: &str = "codex_cli_rs";

/// ChatGPT Responses API client.
pub struct ChatGptResponsesClient {
    http: HttpClient,
}

impl ChatGptResponsesClient {
    /// Create a new ChatGPT Responses client.
    pub fn new(access_token: impl Into<String>, account_id: Option<String>) -> Self {
        let access_token = access_token.into();

        let http = HttpClient::new(CHATGPT_BASE_URL, AuthConfig::Bearer(access_token));

        // Build extra headers for ChatGPT-specific requirements
        let mut extra = HeaderMap::new();
        extra.insert(
            HeaderName::from_static("originator"),
            HeaderValue::from_static(ORIGINATOR),
        );
        let ua = format!(
            "{ORIGINATOR}/{} ({} {}; {})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            "ion"
        );
        if let Ok(value) = HeaderValue::from_str(&ua) {
            extra.insert(reqwest::header::USER_AGENT, value);
        }
        if let Some(account_id) = account_id.as_deref()
            && let Ok(value) = HeaderValue::from_str(account_id)
        {
            extra.insert(HeaderName::from_static("chatgpt-account-id"), value);
        }

        let http = http.with_extra_headers(extra);

        Self { http }
    }

    /// Make a non-streaming responses request.
    pub async fn complete(&self, request: ChatRequest) -> Result<CompletionResponse, Error> {
        let body = build_request(&request, false);

        let value: serde_json::Value = self.http.post_json("/responses", &body).await?;
        let text = extract_output_text(&value);
        Ok(CompletionResponse {
            message: Message {
                role: Role::Assistant,
                content: Arc::new(vec![ContentBlock::Text { text }]),
            },
            usage: Usage::default(),
        })
    }

    /// Stream a responses request.
    pub async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        let body = build_request(&request, true);

        let stream = self.http.post_stream("/responses", &body).await?;
        futures::pin_mut!(stream);

        let mut parser = SseParser::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| Error::Stream(format!("Stream error: {e}")))?;
            let text = String::from_utf8_lossy(&chunk);

            for event in parser.feed(&text) {
                if event.data.is_empty() {
                    continue;
                }

                if let Some(stream_event) =
                    parse_response_event(&event.data, event.event.as_deref())
                {
                    match stream_event {
                        ParsedEvent::TextDelta(delta) => {
                            let _ = tx.send(StreamEvent::TextDelta(delta)).await;
                        }
                        ParsedEvent::ToolCall(call) => {
                            let _ = tx.send(StreamEvent::ToolCall(call)).await;
                        }
                        ParsedEvent::Done => {
                            let _ = tx.send(StreamEvent::Done).await;
                            return Ok(());
                        }
                        ParsedEvent::Error(message) => {
                            return Err(Error::Stream(message));
                        }
                    }
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }
}
