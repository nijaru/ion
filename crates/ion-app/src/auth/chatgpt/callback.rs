//! One bounded listener per authorization attempt; unrelated requests do not
//! consume the callback or expose any pending transaction data.
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use super::{DYNAMIC_CLIENT, SignInError, visible_ascii};

const PATH: &str = "/auth/callback";
const MAX_HEADER: usize = 16 * 1024;
const CONNECTION_DEADLINE: Duration = Duration::from_secs(2);
const AUTHORIZATION_DEADLINE: Duration = Duration::from_secs(300);

pub(super) struct Accepted {
    pub(super) code: String,
    pub(super) client: String,
}

pub(super) async fn bind() -> Result<(TcpListener, String), SignInError> {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| SignInError::CallbackListener)?;
    let addr = listener
        .local_addr()
        .map_err(|_| SignInError::CallbackListener)?;
    Ok((listener, format!("http://{addr}{PATH}")))
}

pub(super) async fn accept(
    listener: &TcpListener,
    state: &str,
    known_client: Option<&str>,
    stop: &CancellationToken,
) -> Result<Accepted, SignInError> {
    tokio::select! {
        biased;
        () = stop.cancelled() => Err(SignInError::Cancelled),
        result = timeout(AUTHORIZATION_DEADLINE, wait(listener, state, known_client)) => {
            result.map_err(|_| SignInError::CallbackTimeout)?
        }
    }
}

async fn wait(
    listener: &TcpListener,
    state: &str,
    known_client: Option<&str>,
) -> Result<Accepted, SignInError> {
    let address = listener
        .local_addr()
        .map_err(|_| SignInError::CallbackListener)?
        .to_string();
    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|_| SignInError::CallbackListener)?;
        let received = timeout(
            CONNECTION_DEADLINE,
            receive(&mut socket, &address, state, known_client),
        )
        .await;
        let pending = match received {
            Ok(Some(outcome)) => Some(outcome),
            Ok(None) => None,
            // A slow or abandoned connection must not consume the attempt.
            Err(_) => continue,
        };
        let reply = match &pending {
            Some(Ok(_)) => response(
                "200 OK",
                "Authorization received. Return to Ion to finish signing in.",
            ),
            Some(Err(SignInError::Denied)) => {
                response("200 OK", "Authorization denied. You can close this tab.")
            }
            _ => response(
                "400 Bad Request",
                "This request does not complete Ion sign-in.",
            ),
        };
        // The browser getting this page is not the authority for accepting a
        // validated callback. Do not discard it if the browser closes early.
        let _ = timeout(CONNECTION_DEADLINE, socket.write_all(reply.as_bytes())).await;
        if let Some(outcome) = pending {
            return outcome;
        }
    }
}

async fn receive(
    socket: &mut TcpStream,
    address: &str,
    state: &str,
    known_client: Option<&str>,
) -> Option<Result<Accepted, SignInError>> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0; 1024];
    loop {
        let length = socket.read(&mut chunk).await.ok()?;
        if length == 0 || length > MAX_HEADER - buffer.len() {
            return None;
        }
        buffer.extend_from_slice(&chunk[..length]);
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut request = httparse::Request::new(&mut headers);
        match request.parse(&buffer) {
            Ok(httparse::Status::Partial) => continue,
            Ok(httparse::Status::Complete(_)) => {
                return parse(&request, address, state, known_client);
            }
            Err(_) => return None,
        }
    }
}

fn parse(
    request: &httparse::Request<'_, '_>,
    address: &str,
    expected: &str,
    known_client: Option<&str>,
) -> Option<Result<Accepted, SignInError>> {
    if request.method != Some("GET") {
        return None;
    }
    let (path, query) = request.path?.split_once('?')?;
    if path != PATH {
        return None;
    }
    let mut hosts = request
        .headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("host"));
    if hosts.next()?.value != address.as_bytes() || hosts.next().is_some() {
        return None;
    }

    let (mut state, mut code, mut client, mut error) = (None, None, None, None);
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let slot = match key.as_ref() {
            "state" => &mut state,
            "code" => &mut code,
            "client_id" => &mut client,
            "error" => &mut error,
            _ => continue,
        };
        if slot.replace(value).is_some() {
            return None;
        }
    }
    if !bool::from(state?.as_bytes().ct_eq(expected.as_bytes())) {
        return None;
    }
    Some((|| {
        if error.as_deref() == Some("access_denied") && code.is_none() {
            return Err(SignInError::Denied);
        }
        if error.is_some() {
            return Err(SignInError::Callback);
        }
        let client = match (known_client, client.as_deref()) {
            (Some(known), None) => known,
            (Some(known), Some(received)) if known == received => known,
            (None, Some(received)) if valid_client(received) => received,
            _ => return Err(SignInError::Callback),
        };
        let code = code
            .filter(|code| visible_ascii(code, 4096))
            .ok_or(SignInError::Callback)?;
        Ok(Accepted {
            code: code.into_owned(),
            client: client.to_owned(),
        })
    })())
}

pub(super) fn valid_client(id: &str) -> bool {
    id != DYNAMIC_CLIENT
        && !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn response(status: &str, message: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\r\n{message}",
        message.len()
    )
}
