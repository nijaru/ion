//! Own HTTP work through startup, invocation retirement and SSE disposal.
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
use tokio::sync::{OwnedRwLockReadGuard, RwLock};
use tokio_util::sync::CancellationToken;

type HttpResult<T> = std::result::Result<T, StreamableHttpError<reqwest::Error>>;

#[derive(Clone)]
pub(super) struct OwnedHttpClient {
    inner: reqwest::Client,
    pub stop: CancellationToken,
    io: Arc<RwLock<()>>,
}

impl OwnedHttpClient {
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
            io: Arc::new(RwLock::new(())),
        })
    }

    async fn interrupt<T>(&self, work: impl Future<Output = HttpResult<T>>) -> HttpResult<T> {
        tokio::select! {
            biased;
            () = self.stop.cancelled() => Err(StreamableHttpError::TransportChannelClosed),
            result = work => result,
        }
    }

    async fn lease(&self) -> HttpResult<OwnedRwLockReadGuard<()>> {
        tokio::select! {
            biased;
            () = self.stop.cancelled() => Err(StreamableHttpError::TransportChannelClosed),
            lease = self.io.clone().read_owned() => Ok(lease),
        }
    }

    pub async fn join_io(&self) {
        // No new I/O can start after stop. Unlike the SDK's abort_all without
        // join_next, this waits for started requests and SSE bodies to drop.
        let _joined = self.io.write().await;
    }

    fn interrupt_stream(
        &self,
        stream: BoxedSseResponse,
        lease: OwnedRwLockReadGuard<()>,
    ) -> BoxedSseResponse {
        Box::pin(
            stream
                .take_until(self.stop.clone().cancelled_owned())
                .map(move |event| {
                    let _keep_owned = &lease;
                    event
                }),
        )
    }

    fn interrupt_response(
        &self,
        response: StreamableHttpPostResponse,
        lease: OwnedRwLockReadGuard<()>,
    ) -> StreamableHttpPostResponse {
        match response {
            StreamableHttpPostResponse::Sse(stream, session) => {
                StreamableHttpPostResponse::Sse(self.interrupt_stream(stream, lease), session)
            }
            response => response,
        }
    }
}

impl StreamableHttpClient for OwnedHttpClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<StreamableHttpPostResponse> {
        let lease = self.lease().await?;
        let response = self
            .interrupt(self.inner.post_message(
                uri,
                message,
                session_id,
                auth_header,
                custom_headers,
            ))
            .await?;
        Ok(self.interrupt_response(response, lease))
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
        let lease = self.lease().await?;
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
        Ok(self.interrupt_response(response, lease))
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<()> {
        // Best-effort remote cleanup remains separate from stopping local I/O.
        // An uncooperative DELETE must not hold local retirement indefinitely.
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            self.inner
                .delete_session(uri, session_id, auth_header, custom_headers),
        )
        .await
        .map_err(|_| StreamableHttpError::TransportChannelClosed)?
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> HttpResult<BoxedSseResponse> {
        let lease = self.lease().await?;
        let stream = self
            .interrupt(self.inner.get_stream(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
            ))
            .await?;
        Ok(self.interrupt_stream(stream, lease))
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
        let lease = self.lease().await?;
        let stream = self
            .interrupt(self.inner.get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            ))
            .await?;
        Ok(self.interrupt_stream(stream, lease))
    }
}
