//! Interrupt only failed HTTP startup, including an idle bootstrap SSE body.
use std::{collections::HashMap, future::Future, sync::Arc};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::{
    model::ClientJsonRpcMessage,
    transport::{
        common::client_side_sse::BoxedSseResponse,
        streamable_http_client::{
            StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
        },
    },
};
use tokio_util::sync::CancellationToken;

type HttpResult<T> = std::result::Result<T, StreamableHttpError<reqwest::Error>>;

#[derive(Clone)]
pub(super) struct StartupHttpClient {
    inner: reqwest::Client,
    pub stop: CancellationToken,
}

impl StartupHttpClient {
    pub fn new() -> Result<Self> {
        // Match rmcp's reqwest defaults. Redirects must not replay supplied
        // credentials/custom headers to another destination.
        let inner = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("cannot build MCP HTTP client")?;
        Ok(Self {
            inner,
            stop: CancellationToken::new(),
        })
    }

    async fn interrupt<T>(&self, work: impl Future<Output = HttpResult<T>>) -> HttpResult<T> {
        tokio::select! {
            biased;
            () = self.stop.cancelled() => Err(StreamableHttpError::TransportChannelClosed),
            result = work => result,
        }
    }

    fn interrupt_response(
        &self,
        response: StreamableHttpPostResponse,
    ) -> StreamableHttpPostResponse {
        match response {
            StreamableHttpPostResponse::Sse(stream, session) => StreamableHttpPostResponse::Sse(
                Box::pin(stream.take_until(self.stop.clone().cancelled_owned())),
                session,
            ),
            response => response,
        }
    }
}

impl StreamableHttpClient for StartupHttpClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<StreamableHttpPostResponse> {
        let response = self
            .interrupt(self.inner.post_message(
                uri,
                message,
                session_id,
                auth_header,
                custom_headers,
            ))
            .await?;
        Ok(self.interrupt_response(response))
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> HttpResult<StreamableHttpPostResponse> {
        let response = self
            .interrupt(self.inner.post_message_with_max_sse_event_size(
                uri,
                message,
                session_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            ))
            .await?;
        Ok(self.interrupt_response(response))
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<()> {
        // GET and DELETE are outside bootstrap; retain SDK cancellation and
        // session cleanup semantics rather than stopping them with startup.
        self.inner
            .delete_session(uri, session_id, auth_header, custom_headers)
            .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<BoxedSseResponse> {
        self.inner
            .get_stream(uri, session_id, last_event_id, auth_header, custom_headers)
            .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> HttpResult<BoxedSseResponse> {
        self.inner
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            )
            .await
    }
}
