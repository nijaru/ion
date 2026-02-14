//! Anthropic Messages API client.

use super::convert;
use super::stream::StreamEvent as AnthropicStreamEvent;
use crate::provider::error::Error;
use crate::provider::http::{AuthConfig, HttpClient, SseParser};
use crate::provider::stream::ToolCallAccumulator;
use crate::provider::types::{
    ChatRequest, CompletionResponse, StreamEvent, Usage as IonUsage,
};
use futures::StreamExt;
use tokio::sync::mpsc;

const BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Native Anthropic Messages API client.
pub struct AnthropicClient {
    http: HttpClient,
}

impl AnthropicClient {
    /// Create a new Anthropic client.
    pub fn new(api_key: impl Into<String>) -> Self {
        let api_key = api_key.into();
        let mut http = HttpClient::new(
            BASE_URL,
            AuthConfig::ApiKey {
                header: "x-api-key".to_string(),
                key: api_key,
            },
        );

        // Add Anthropic-specific headers
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(version) = reqwest::header::HeaderValue::from_str(API_VERSION) {
            headers.insert("anthropic-version", version);
        }
        http = http.with_extra_headers(headers);

        Self { http }
    }

    /// Make a non-streaming chat completion request.
    pub async fn complete(&self, request: ChatRequest) -> Result<CompletionResponse, Error> {
        let api_request = convert::build_request(&request, false);

        tracing::debug!(
            model = %api_request.model,
            messages = api_request.messages.len(),
            tools = api_request.tools.as_ref().map_or(0, std::vec::Vec::len),
            "Anthropic API request"
        );

        let response: super::response::AnthropicResponse =
            self.http.post_json("/v1/messages", &api_request).await?;

        let usage = IonUsage {
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
            cache_read_tokens: response.usage.cache_read_input_tokens,
            cache_write_tokens: response.usage.cache_creation_input_tokens,
        };

        Ok(CompletionResponse {
            message: convert::convert_response(response),
            usage,
        })
    }

    /// Stream a chat completion request.
    pub async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        let api_request = convert::build_request(&request, true);

        tracing::debug!(
            model = %api_request.model,
            messages = api_request.messages.len(),
            tools = api_request.tools.as_ref().map_or(0, std::vec::Vec::len),
            "Anthropic API stream request"
        );

        let stream = self.http.post_stream("/v1/messages", &api_request).await?;
        futures::pin_mut!(stream);

        let mut parser = SseParser::new();
        let mut tools = ToolCallAccumulator::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| Error::Stream(e.to_string()))?;
            let text = String::from_utf8_lossy(&chunk);

            for sse_event in parser.feed(&text) {
                if sse_event.data.is_empty() || sse_event.data == "[DONE]" {
                    continue;
                }

                match serde_json::from_str::<AnthropicStreamEvent>(&sse_event.data) {
                    Ok(event) => {
                        if let Err(e) =
                            convert::handle_stream_event(event, &tx, &mut tools).await
                        {
                            let _ = tx.send(StreamEvent::Error(e.to_string())).await;
                            return Err(e);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to parse Anthropic event: {e}\nData: {}",
                            sse_event.data
                        );
                    }
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }
}
