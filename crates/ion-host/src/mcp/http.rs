//! Own HTTP work through startup, invocation retirement and SSE disposal.
use std::{
    borrow::Cow,
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest::{
    StatusCode,
    header::{ACCEPT, HeaderName, HeaderValue, WWW_AUTHENTICATE},
};
use rmcp::{
    model::{ClientJsonRpcMessage, ClientRequest, ErrorData, JsonRpcMessage, ServerJsonRpcMessage},
    transport::{
        common::{
            client_side_sse::BoxedSseResponse,
            http_header::{EVENT_STREAM_MIME_TYPE, HEADER_SESSION_ID, JSON_MIME_TYPE},
        },
        streamable_http_client::{
            AuthRequiredError, InsufficientScopeError, StreamableHttpClient, StreamableHttpError,
            StreamableHttpPostResponse,
        },
    },
};
use tokio::sync::{OwnedRwLockReadGuard, RwLock};
use tokio_util::sync::CancellationToken;

use super::framing::{DEFAULT_MAX_SSE_EVENT_BYTES, MAX_MCP_MESSAGE_BYTES, bounded_sse_events};

type HttpResult<T> = std::result::Result<T, StreamableHttpError<reqwest::Error>>;

#[derive(Clone, Copy)]
struct PostLimits {
    body: usize,
    sse: usize,
}

#[derive(Clone)]
pub(super) struct OwnedHttpClient {
    inner: reqwest::Client,
    pub stop: CancellationToken,
    io: Arc<RwLock<()>>,
    failure: Arc<Mutex<Option<String>>>,
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
            failure: Arc::new(Mutex::new(None)),
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

    /// A framing failure retires this connection: the SDK treats stream errors
    /// as resumable unless they are its private fatal type, so a rejected event
    /// must not leave a resume GET replaying it. Keep the reason for the caller.
    fn fail(&self, reason: String) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        failure.get_or_insert(reason);
        self.stop.cancel();
    }

    pub fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
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

    async fn post_with_limits(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        limits: PostLimits,
    ) -> HttpResult<StreamableHttpPostResponse> {
        let mut request = self
            .inner
            .post(uri.as_ref())
            .header(ACCEPT, [EVENT_STREAM_MIME_TYPE, JSON_MIME_TYPE].join(", "));
        if let Some(auth_header) = auth_header {
            request = request.bearer_auth(auth_header);
        }
        request = apply_custom_headers(request, custom_headers)?;
        let session_was_attached = session_id.is_some();
        if let Some(session_id) = session_id {
            request = request.header(HEADER_SESSION_ID, session_id.as_ref());
        }
        let response = request.json(&message).send().await?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED
            && let Some(header) = response.headers().get(WWW_AUTHENTICATE)
        {
            let header = header
                .to_str()
                .map_err(|_| {
                    StreamableHttpError::UnexpectedServerResponse(Cow::from(
                        "invalid www-authenticate header value",
                    ))
                })?
                .to_string();
            return Err(StreamableHttpError::AuthRequired(AuthRequiredError::new(
                header,
            )));
        }
        if status == StatusCode::FORBIDDEN
            && let Some(header) = response.headers().get(WWW_AUTHENTICATE)
        {
            let header = header
                .to_str()
                .map_err(|_| {
                    StreamableHttpError::UnexpectedServerResponse(Cow::from(
                        "invalid www-authenticate header value",
                    ))
                })?
                .to_string();
            let required_scope = extract_scope_from_header(&header);
            return Err(StreamableHttpError::InsufficientScope(
                InsufficientScopeError::new(header, required_scope),
            ));
        }
        if matches!(status, StatusCode::ACCEPTED | StatusCode::NO_CONTENT) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status == StatusCode::NOT_FOUND && session_was_attached {
            return Err(StreamableHttpError::SessionExpired);
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .map(|content_type| String::from_utf8_lossy(content_type.as_bytes()).to_string());
        let session_id = response
            .headers()
            .get(HEADER_SESSION_ID)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        // Spec requires 202 Accepted for notifications, but some servers send
        // an empty 200. Keep that accepted before treating the body as JSON.
        if status.is_success()
            && response.content_length() == Some(0)
            && matches!(
                message,
                ClientJsonRpcMessage::Notification(_)
                    | ClientJsonRpcMessage::Response(_)
                    | ClientJsonRpcMessage::Error(_)
            )
        {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if !status.is_success() {
            let body = read_bounded_body(response, limits.body).await?;
            let body = String::from_utf8_lossy(&body);
            if let Some(response) =
                legacy_discover_response(&message, session_was_attached, status, &body)
            {
                return Ok(response);
            }
            if content_type.as_deref().is_some_and(is_json)
                && let Some(message) = parse_json_rpc_error(&body)
            {
                return Ok(StreamableHttpPostResponse::Json(message, session_id));
            }
            return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
                format!("HTTP {status}: {}", body_preview(body.as_bytes())),
            )));
        }
        match content_type.as_deref() {
            Some(content_type) if is_event_stream(content_type) => {
                Ok(StreamableHttpPostResponse::Sse(
                    self.bounded_sse_response(response, limits.sse),
                    session_id,
                ))
            }
            Some(content_type) if is_json(content_type) => {
                // A notification/response/error POST does not await a JSON-RPC
                // reply. Treat an unusable JSON body as Accepted. A request
                // still needs a reply, so the same body is an error.
                let body = read_bounded_body(response, limits.body).await?;
                match serde_json::from_slice::<ServerJsonRpcMessage>(&body) {
                    Ok(parsed) => Ok(StreamableHttpPostResponse::Json(parsed, session_id)),
                    Err(_)
                        if matches!(
                            message,
                            ClientJsonRpcMessage::Notification(_)
                                | ClientJsonRpcMessage::Response(_)
                                | ClientJsonRpcMessage::Error(_)
                        ) =>
                    {
                        Ok(StreamableHttpPostResponse::Accepted)
                    }
                    Err(error) => Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
                        json_rpc_parse_error_message(&error, &body),
                    ))),
                }
            }
            _ => Err(StreamableHttpError::UnexpectedContentType(content_type)),
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
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_header,
            custom_headers,
            DEFAULT_MAX_SSE_EVENT_BYTES,
        )
        .await
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
            .interrupt(self.post_with_limits(
                uri,
                message,
                session_id,
                auth_header,
                custom_headers,
                PostLimits {
                    body: MAX_MCP_MESSAGE_BYTES,
                    sse: max_sse_event_size.min(MAX_MCP_MESSAGE_BYTES),
                },
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
        self.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            auth_header,
            custom_headers,
            DEFAULT_MAX_SSE_EVENT_BYTES,
        )
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
        let lease = self.lease().await?;
        let stream = self
            .interrupt(self.inner.get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
                max_sse_event_size.min(MAX_MCP_MESSAGE_BYTES),
            ))
            .await?;
        Ok(self.interrupt_stream(stream, lease))
    }
}

fn apply_custom_headers(
    mut builder: reqwest::RequestBuilder,
    custom_headers: HashMap<HeaderName, HeaderValue>,
) -> HttpResult<reqwest::RequestBuilder> {
    for (name, value) in custom_headers {
        // Mirrors the SDK's reserved names. MCP-Protocol-Version is reserved
        // but allowed because the worker injects it after initialization.
        if ["accept", "mcp-session-id", "last-event-id"]
            .iter()
            .any(|reserved| name.as_str().eq_ignore_ascii_case(reserved))
        {
            return Err(StreamableHttpError::ReservedHeaderConflict(
                name.to_string(),
            ));
        }
        builder = builder.header(name, value);
    }
    Ok(builder)
}

fn extract_scope_from_header(header: &str) -> Option<String> {
    let header_lowercase = header.to_ascii_lowercase();
    let start = header_lowercase.find("scope=")? + "scope=".len();
    let value = &header[start..];
    if let Some(stripped) = value.strip_prefix('"') {
        let end_quote = stripped.find('"')?;
        Some(stripped[..end_quote].to_owned())
    } else {
        let end = value
            .find(|character: char| {
                character == ',' || character == ';' || character.is_whitespace()
            })
            .unwrap_or(value.len());
        (end > 0).then(|| value[..end].to_owned())
    }
}

fn is_event_stream(content_type: &str) -> bool {
    content_type
        .as_bytes()
        .starts_with(EVENT_STREAM_MIME_TYPE.as_bytes())
}

fn is_json(content_type: &str) -> bool {
    content_type
        .as_bytes()
        .starts_with(JSON_MIME_TYPE.as_bytes())
}

fn parse_json_rpc_error(body: &str) -> Option<ServerJsonRpcMessage> {
    match serde_json::from_str::<ServerJsonRpcMessage>(body) {
        Ok(message @ JsonRpcMessage::Error(_)) => Some(message),
        _ => None,
    }
}

fn legacy_discover_response(
    message: &ClientJsonRpcMessage,
    session_was_attached: bool,
    status: StatusCode,
    body: &str,
) -> Option<StreamableHttpPostResponse> {
    if session_was_attached
        || !status.is_client_error()
        || matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
    {
        return None;
    }
    let ClientJsonRpcMessage::Request(request) = message else {
        return None;
    };
    if !matches!(request.request, ClientRequest::DiscoverRequest(_)) {
        return None;
    }
    let error = match serde_json::from_str::<ServerJsonRpcMessage>(body) {
        Ok(ServerJsonRpcMessage::Error(error)) => error.error,
        _ => ErrorData::invalid_request(
            format!(
                "server/discover rejected with HTTP {status}: {}",
                body_preview(body.as_bytes())
            ),
            None,
        ),
    };
    Some(StreamableHttpPostResponse::Json(
        ServerJsonRpcMessage::error(error, Some(request.id.clone())),
        None,
    ))
}

fn json_rpc_parse_error_message(error: &serde_json::Error, body: &[u8]) -> String {
    format!(
        "could not parse JSON response as ServerJsonRpcMessage: {error}: {}",
        body_preview(body)
    )
}

fn body_preview(body: &[u8]) -> Cow<'static, str> {
    const PREVIEW_BYTES: usize = 256;
    if body.is_empty() {
        return Cow::Borrowed("<empty>");
    }
    // Decode only the preview prefix so an invalid 64 MiB body cannot expand
    // into a larger replacement string merely to produce a diagnostic.
    let lossy = String::from_utf8_lossy(&body[..body.len().min(PREVIEW_BYTES)]);
    if lossy.len() <= PREVIEW_BYTES {
        return Cow::Owned(lossy.into_owned());
    }
    let mut end = PREVIEW_BYTES;
    while end > 0 && !lossy.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Owned(lossy[..end].to_owned())
}

fn body_too_large(limit: usize) -> StreamableHttpError<reqwest::Error> {
    StreamableHttpError::UnexpectedServerResponse(Cow::Owned(format!(
        "HTTP response body exceeded the maximum size of {limit} bytes"
    )))
}

async fn read_bounded_body(response: reqwest::Response, limit: usize) -> HttpResult<Vec<u8>> {
    if let Some(length) = response.content_length()
        && length > limit as u64
    {
        return Err(body_too_large(limit));
    }
    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(limit);
    let mut body = Vec::with_capacity(capacity);
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(body_too_large(limit));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl OwnedHttpClient {
    /// Bound POST SSE bytes before parsing. A rejected event retires this
    /// connection: the SDK treats foreign stream errors as resumable, and a
    /// resume GET could replay the event that framing already refused.
    fn bounded_sse_response(
        &self,
        response: reqwest::Response,
        max_event_size: usize,
    ) -> BoxedSseResponse {
        let client = self.clone();
        let stream = bounded_sse_events(response.bytes_stream(), max_event_size).map(move |item| {
            if let Err(error) = &item {
                client.fail(format!("MCP HTTP response framing failed: {error}"));
            }
            item
        });
        sse_stream::SseStream::from_bytes_stream(stream).boxed()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{McpConfig, McpExposure, McpHttpServer, McpServer, McpTools};
    use std::{fs, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };

    async fn read_request(socket: &mut TcpStream) {
        let mut data = Vec::new();
        let mut buffer = [0_u8; 2048];
        let header_end = loop {
            let read = socket.read(&mut buffer).await.unwrap();
            assert!(read > 0, "request closed before its headers");
            data.extend_from_slice(&buffer[..read]);
            if let Some(offset) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let head = String::from_utf8_lossy(&data[..header_end]);
        let mut length = 0;
        for line in head.split("\r\n") {
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        while data.len() < header_end + length {
            let read = socket.read(&mut buffer).await.unwrap();
            assert!(read > 0, "request closed before its body");
            data.extend_from_slice(&buffer[..read]);
        }
    }

    async fn spawn_response(response: Vec<u8>) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            // A client that rejects the declared body may close before this
            // write completes; that is the behavior under test.
            let _ = socket.write_all(&response).await;
            let _ = socket.shutdown().await;
        });
        (format!("http://{address}/mcp"), server)
    }

    fn request(method: &str, id: i64) -> ClientJsonRpcMessage {
        serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": {},
        }))
        .unwrap()
    }

    async fn post(
        url: &str,
        message: ClientJsonRpcMessage,
        session: Option<&str>,
        body_limit: usize,
        sse_limit: usize,
    ) -> HttpResult<StreamableHttpPostResponse> {
        OwnedHttpClient::new()
            .unwrap()
            .post_with_limits(
                Arc::from(url),
                message,
                session.map(Arc::from),
                None,
                HashMap::new(),
                PostLimits {
                    body: body_limit,
                    sse: sse_limit,
                },
            )
            .await
    }

    fn assert_body_too_large(result: HttpResult<StreamableHttpPostResponse>) {
        match result {
            Err(StreamableHttpError::UnexpectedServerResponse(message)) => {
                assert!(message.contains("exceeded the maximum size"), "{message}");
            }
            other => panic!("expected a bounded-body transport error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chunked_success_and_declared_error_bodies_stop_before_decode() {
        let message = request("ping", 1);
        let chunked = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n8\r\n12345678\r\n0\r\n\r\n".to_vec();
        let (url, server) = spawn_response(chunked).await;
        let result = post(&url, message.clone(), None, 7, 1024).await;
        server.await.unwrap();
        assert_body_too_large(result);

        let declared = b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: 9\r\nConnection: close\r\n\r\n".to_vec();
        let (url, server) = spawn_response(declared).await;
        let result = post(&url, message, None, 8, 1024).await;
        server.await.unwrap();
        assert_body_too_large(result);
    }

    #[tokio::test]
    async fn sse_wire_budget_rejects_an_oversize_event_before_parsing() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: 1234567890\n\n".to_vec();
        let (url, server) = spawn_response(response).await;
        let client = OwnedHttpClient::new().unwrap();
        let result = client
            .post_with_limits(
                Arc::from(url.as_str()),
                request("ping", 1),
                None,
                None,
                HashMap::new(),
                PostLimits { body: 1024, sse: 4 },
            )
            .await;
        server.await.unwrap();
        let StreamableHttpPostResponse::Sse(mut stream, _) = result.unwrap() else {
            panic!("expected an SSE response");
        };
        let error = stream.next().await.unwrap().unwrap_err();
        assert!(error.to_string().contains("SSE event exceeded"), "{error}");
        // The SDK treats foreign stream errors as resumable; a rejected event
        // must retire this connection instead of replaying it on a resume GET.
        assert!(
            client.stop.is_cancelled(),
            "framing failure left the connection live"
        );
        let failure = client.failure().expect("framing failure reason");
        assert!(failure.contains("framing failed"), "{failure}");
        // The connection is retired, so every in-flight caller observes the
        // same reason instead of racing for a consumable value.
        assert_eq!(client.failure().as_deref(), Some(failure.as_str()));
    }

    #[tokio::test]
    async fn post_adapter_preserves_auth_and_session_failures() {
        let auth = b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer realm=\"fixture\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (url, server) = spawn_response(auth).await;
        let result = post(&url, request("ping", 1), None, 1024, 1024).await;
        server.await.unwrap();
        match result {
            Err(StreamableHttpError::AuthRequired(error)) => {
                assert_eq!(error.www_authenticate_header, "Bearer realm=\"fixture\"");
            }
            other => panic!("expected an auth challenge, got {other:?}"),
        }

        let scope = b"HTTP/1.1 403 Forbidden\r\nWWW-Authenticate: Bearer error=\"insufficient_scope\", scope=\"files:read files:write\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (url, server) = spawn_response(scope).await;
        let result = post(&url, request("ping", 1), None, 1024, 1024).await;
        server.await.unwrap();
        match result {
            Err(StreamableHttpError::InsufficientScope(error)) => {
                assert_eq!(
                    error.required_scope.as_deref(),
                    Some("files:read files:write")
                );
            }
            other => panic!("expected a scope challenge, got {other:?}"),
        }

        let missing =
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (url, server) = spawn_response(missing).await;
        let result = post(&url, request("ping", 1), Some("session"), 1024, 1024).await;
        server.await.unwrap();
        assert!(
            matches!(result, Err(StreamableHttpError::SessionExpired)),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn post_adapter_preserves_accepted_error_json_and_discover_fallback() {
        let accepted =
            b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let (url, server) = spawn_response(accepted).await;
        let result = post(&url, request("ping", 1), None, 1024, 1024).await;
        server.await.unwrap();
        assert!(
            matches!(result, Ok(StreamableHttpPostResponse::Accepted)),
            "{result:?}"
        );

        let error_body = br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"fixture"}}"#;
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            error_body.len()
        );
        let mut response = response.into_bytes();
        response.extend_from_slice(error_body);
        let (url, server) = spawn_response(response).await;
        let result = post(&url, request("ping", 1), None, 1024, 1024).await;
        server.await.unwrap();
        let StreamableHttpPostResponse::Json(message, None) = result.unwrap() else {
            panic!("expected a JSON-RPC error response");
        };
        let value = serde_json::to_value(message).unwrap();
        assert_eq!(value["error"]["message"], "fixture");

        let response =
            b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 6\r\nConnection: close\r\n\r\nlegacy".to_vec();
        let (url, server) = spawn_response(response).await;
        let result = post(&url, request("server/discover", 7), None, 1024, 1024).await;
        server.await.unwrap();
        let StreamableHttpPostResponse::Json(message, None) = result.unwrap() else {
            panic!("expected a synthesized discover rejection");
        };
        let value = serde_json::to_value(message).unwrap();
        assert_eq!(value["id"], 7);
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("server/discover rejected with HTTP 400"),
            "{value}"
        );
    }

    #[tokio::test]
    async fn declared_json_body_over_actual_message_budget_fails_startup_without_reading() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nMcp-Session-Id: fixture\r\nConnection: close\r\n\r\n",
                MAX_MCP_MESSAGE_BYTES + 1
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            let mut byte = [0_u8; 1];
            socket
                .read(&mut byte)
                .await
                .map(|read| read == 0)
                .unwrap_or(true)
        });
        let root = std::env::temp_dir().join(format!("ion-mcp-body-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let config = McpConfig::new(&root);
        config
            .add(
                "oversize",
                McpServer::Http(McpHttpServer {
                    enabled: true,
                    description: None,
                    url: format!("http://{address}/mcp"),
                    bearer_token_env: None,
                    headers: Default::default(),
                    exposure: McpExposure::Direct,
                    tool_exposure: Default::default(),
                }),
            )
            .unwrap();
        let startup =
            tokio::time::timeout(Duration::from_secs(10), McpTools::connect(&config, &root))
                .await
                .unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(closed, "client kept reading a declared oversize JSON body");
        assert!(startup.tools.is_none());
        assert_eq!(startup.diagnostics.len(), 1, "{:?}", startup.diagnostics);
        assert!(
            startup.diagnostics[0].contains("exceeded the maximum size"),
            "{:?}",
            startup.diagnostics
        );
    }
}
