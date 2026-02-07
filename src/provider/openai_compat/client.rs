//! OpenAI-compatible API client.

use super::quirks::ProviderQuirks;
use super::request_builder::build_request;
use super::stream::StreamChunk;
use super::stream_handler::{convert_response, handle_stream_chunk};
use crate::provider::api_provider::Provider;
use crate::provider::error::Error;
use crate::provider::http::{AuthConfig, HttpClient, SseParser};
use crate::provider::prefs::ProviderPrefs;
use crate::provider::types::{ChatRequest, Message, StreamEvent, ToolBuilder};
use futures::StreamExt;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Native OpenAI-compatible API client.
pub struct OpenAICompatClient {
    http: HttpClient,
    quirks: ProviderQuirks,
    provider: Provider,
}

impl OpenAICompatClient {
    /// Create a new OpenAI-compatible client.
    #[allow(clippy::unnecessary_wraps)]
    pub fn new(provider: Provider, api_key: impl Into<String>) -> Result<Self, Error> {
        let quirks = ProviderQuirks::for_provider(provider);
        let api_key = api_key.into();

        // Local provider doesn't need auth
        let auth = if provider == Provider::Local {
            AuthConfig::Bearer(String::new())
        } else {
            AuthConfig::Bearer(api_key)
        };

        // Allow env var override for local provider URL
        let base_url = if provider == Provider::Local {
            std::env::var("ION_LOCAL_URL")
                .unwrap_or_else(|_| quirks.base_url.to_string())
        } else {
            quirks.base_url.to_string()
        };

        let http = HttpClient::new(base_url, auth);

        Ok(Self {
            http,
            quirks,
            provider,
        })
    }


    /// Create a client with custom base URL.
    #[allow(clippy::unnecessary_wraps)]
    pub fn with_base_url(
        provider: Provider,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, Error> {
        let quirks = ProviderQuirks::for_provider(provider);
        let api_key = api_key.into();

        let auth = if provider == Provider::Local {
            AuthConfig::Bearer(String::new())
        } else {
            AuthConfig::Bearer(api_key)
        };

        let http = HttpClient::new(base_url.into(), auth);

        Ok(Self {
            http,
            quirks,
            provider,
        })
    }

    /// Make a non-streaming chat completion request.
    pub async fn complete(&self, request: ChatRequest) -> Result<Message, Error> {
        let api_request = build_request(&request, None, false, &self.quirks);

        tracing::debug!(
            provider = %self.provider.id(),
            model = %api_request.model,
            messages = api_request.messages.len(),
            tools = api_request.tools.as_ref().map_or(0, std::vec::Vec::len),
            "OpenAI-compat API request"
        );

        let response = self
            .http
            .post_json("/chat/completions", &api_request)
            .await?;

        Ok(convert_response(&response, &self.quirks))
    }

    /// Stream a chat completion request.
    pub async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        self.stream_with_prefs(request, None, tx).await
    }

    /// Stream with provider preferences (for `OpenRouter` routing).
    pub async fn stream_with_prefs(
        &self,
        request: ChatRequest,
        prefs: Option<&ProviderPrefs>,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        let api_request = build_request(&request, prefs, true, &self.quirks);

        tracing::debug!(
            provider = %self.provider.id(),
            model = %api_request.model,
            messages = api_request.messages.len(),
            tools = api_request.tools.as_ref().map_or(0, std::vec::Vec::len),
            has_routing = api_request.provider.is_some(),
            "OpenAI-compat API stream request"
        );

        let stream = self
            .http
            .post_stream("/chat/completions", &api_request)
            .await?;
        futures::pin_mut!(stream);

        let mut parser = SseParser::new();
        let mut tool_builders: HashMap<usize, ToolBuilder> = HashMap::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| Error::Stream(e.to_string()))?;
            let text = String::from_utf8_lossy(&chunk);

            for sse_event in parser.feed(&text) {
                if sse_event.data.is_empty() || sse_event.data == "[DONE]" {
                    continue;
                }

                match serde_json::from_str::<StreamChunk>(&sse_event.data) {
                    Ok(chunk) => {
                        handle_stream_chunk(chunk, &tx, &mut tool_builders, &self.quirks).await?;
                    }
                    Err(e) => {
                        // Check if it's an error response
                        if let Some(msg) =
                            serde_json::from_str::<serde_json::Value>(&sse_event.data)
                                .ok()
                                .and_then(|v| {
                                    v.get("error")?.get("message")?.as_str().map(String::from)
                                })
                        {
                            return Err(Error::Api(msg));
                        }
                        tracing::warn!(
                            "Failed to parse OpenAI chunk: {e}\nData: {}",
                            sse_event.data
                        );
                    }
                }
            }
        }

        // Emit any remaining tool calls
        for (_, builder) in tool_builders {
            if let Some(call) = builder.finish() {
                let _ = tx.send(StreamEvent::ToolCall(call)).await;
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }

    /// Build request for testing purposes.
    #[cfg(test)]
    pub(crate) fn build_request_for_test(
        &self,
        request: &ChatRequest,
        prefs: Option<&ProviderPrefs>,
        stream: bool,
    ) -> super::request::OpenAIRequest {
        build_request(request, prefs, stream, &self.quirks)
    }
}
