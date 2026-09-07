//! OpenRouter OAuth PKCE flow (pi's `auth/oauth/openrouter.ts`).
//!
//! OpenRouter exchanges an authorization code for a permanent,
//! user-controlled API key rather than an expiring token pair. The
//! callback lands on a one-shot loopback listener on an ephemeral
//! port; the browser opens the authorize URL; the login view also
//! accepts a pasted code or redirect URL for headless sessions.

use std::time::Duration;

use base64::Engine as _;

use super::{Credential, LoginError};

const AUTHORIZE_URL: &str = "https://openrouter.ai/auth";
const TOKEN_URL: &str = "https://openrouter.ai/api/v1/auth/keys";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// PKCE pair (verifier + S256 challenge), base64url without padding.
pub(super) struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub(super) fn generate_pkce() -> Result<Pkce, LoginError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|err| LoginError::Exchange(format!("PKCE randomness: {err}")))?;
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    Ok(Pkce {
        verifier,
        challenge,
    })
}

/// Extract the `code` query parameter from a pasted URL or a bare
/// code string (pi's parseAuthorizationInput).
#[must_use]
pub fn parse_code(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Ok(url) = reqwest::Url::parse(input)
        && let Some(code) = url.query_pairs().find(|(k, _)| k == "code")
    {
        return Some(code.1.to_string());
    }
    if input.contains("code=") {
        let params: Vec<&str> = input.split('&').collect();
        for part in params {
            if let Some(code) = part.strip_prefix("code=") {
                return Some(code.to_owned());
            }
        }
    }
    Some(input.to_owned())
}

/// The URL the user visits plus the loopback listener that finishes
/// the login. Dropping the `LoginListener` stops the server.
pub struct LoginListener {
    pub authorize_url: String,
    pub callback_url: String,
    verifier: String,
    cancel: tokio_util::sync::CancellationToken,
    code_rx: Option<tokio::sync::oneshot::Receiver<Result<String, String>>>,
}

impl LoginListener {
    /// Start the flow: bind the loopback listener and build the
    /// authorize URL. Call `wait_for_code` after showing the URL.
    pub async fn start() -> Result<Self, LoginError> {
        let pkce = generate_pkce()?;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|err| LoginError::Bind(err.to_string()))?;
        let port = listener
            .local_addr()
            .map_err(|err| LoginError::Bind(err.to_string()))?
            .port();
        let callback_path = format!("/oauth/callback/{}", uuid_placeholder());
        let callback_url = format!("http://127.0.0.1:{port}{callback_path}");

        let authorize_url = format!(
            "{AUTHORIZE_URL}?callback_url={callback_url}&code_challenge={}&code_challenge_method=S256",
            pkce.challenge,
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        let (code_tx, code_rx) = tokio::sync::oneshot::channel();
        let task_cancel = cancel.clone();
        tokio::spawn(async move {
            // One accepted callback settles the flow; the task exits
            // with the listener when cancelled.
            tokio::select! {
                _ = task_cancel.cancelled() => {}
                accepted = accept_callback(listener, &callback_path) => {
                    let _ = code_tx.send(accepted);
                }
            }
        });

        Ok(Self {
            authorize_url,
            callback_url,
            verifier: pkce.verifier,
            cancel,
            code_rx: Some(code_rx),
        })
    }

    /// The PKCE verifier for the code exchange.
    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    /// Await the browser callback (or an error page). `timeout`
    /// bounds the wait; cancel by dropping the listener.
    pub async fn wait_for_code(mut self) -> Result<String, LoginError> {
        let code_rx = self
            .code_rx
            .take()
            .expect("the code receiver is taken exactly once");
        let code = tokio::time::timeout(LOGIN_TIMEOUT, code_rx)
            .await
            .map_err(|_| LoginError::Timeout)?
            .map_err(|_| LoginError::Cancelled)?
            .map_err(LoginError::Provider)?;
        Ok(code)
    }
}

impl Drop for LoginListener {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Accept one GET on the callback path, answer with a minimal page,
/// and return the authorization code (pi answers success/error HTML).
async fn accept_callback(
    listener: tokio::net::TcpListener,
    callback_path: &str,
) -> Result<String, String> {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return Err("the callback listener stopped".to_owned());
        };
        let Ok(request) = read_request_head(&mut stream).await else {
            continue;
        };
        if let Some(error) = query_param(&request, "error") {
            let _ = respond(&mut stream, 400, "Authorization failed.").await;
            return Err(format!(
                "authorization failed: {}",
                query_param(&request, "error_description").unwrap_or(error)
            ));
        }
        if !request.starts_with("GET") || !request_path(&request).starts_with(callback_path) {
            _ = respond(&mut stream, 404, "Callback route not found.").await;
            continue;
        }
        let Some(code) = query_param(&request, "code") else {
            _ = respond(&mut stream, 400, "Missing authorization code.").await;
            return Err("OpenRouter returned no authorization code".to_owned());
        };
        _ = respond(
            &mut stream,
            200,
            "Signed in to OpenRouter. You may now close this page.",
        )
        .await;
        return Ok(code);
    }
}

/// Read the request head (method + path + headers), stopping at the
/// blank line. Shared with the Codex callback listener.
pub(super) async fn read_request_head(
    stream: &mut tokio::net::TcpStream,
) -> Result<String, std::io::Error> {
    use tokio::io::AsyncReadExt as _;
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut chunk = [0u8; 256];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 8192 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn request_path(request: &str) -> &str {
    request.split_whitespace().nth(1).unwrap_or("")
}

/// Query parameter lookup shared with the Codex callback listener.
pub(super) fn query_param(request: &str, name: &str) -> Option<String> {
    let path = request_path(request);
    let query = path.split_once('?')?.1;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == name {
            return Some(v.to_owned());
        }
    }
    None
}

/// Write one minimal HTTP response. Shared with the Codex listener.
pub(super) async fn respond(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> Result<(), std::io::Error> {
    use tokio::io::AsyncWriteExt as _;
    let reason = if status == 200 { "OK" } else { "Error" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await
}

fn uuid_placeholder() -> String {
    // A random, non-guessable callback path suffix (pi uses a UUID).
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("system randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Exchange the authorization code for the permanent API key.
pub async fn exchange_key(code: &str, verifier: &str) -> Result<Credential, LoginError> {
    let client = reqwest::Client::new();
    let response = client
        .post(TOKEN_URL)
        .json(&serde_json::json!({
            "code": code,
            "code_verifier": verifier,
            "code_challenge_method": "S256",
        }))
        .send()
        .await
        .map_err(|err| LoginError::Exchange(err.to_string()))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|err| LoginError::Exchange(format!("invalid token response: {err}")))?;
    if !status.is_success() {
        let detail = body
            .get("error_description")
            .or_else(|| body.get("message"))
            .or_else(|| body.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return Err(LoginError::Exchange(format!(
            "OpenRouter key exchange failed (HTTP {status}): {detail}"
        )));
    }
    let key = body
        .get("key")
        .and_then(|v| v.as_str())
        .filter(|k| !k.is_empty())
        .ok_or_else(|| LoginError::Exchange("response carries no \"key\"".to_owned()))?;
    Ok(Credential::Oauth {
        access: key.to_owned(),
        // A permanent, user-controlled key: pi stores MAX_SAFE_INTEGER.
        refresh: String::new(),
        expires: 9_007_199_254_740_991,
        account_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_is_base64url_s256() {
        let pkce = generate_pkce().expect("pkce");
        assert!(!pkce.verifier.contains(['+', '/', '=']));
        assert_eq!(pkce.challenge.len(), 43);
    }

    #[test]
    fn parses_codes_from_urls_and_bare_input() {
        assert_eq!(
            parse_code("http://127.0.0.1:1234/cb?code=ABC&state=x"),
            Some("ABC".to_owned())
        );
        assert_eq!(parse_code("code=DEF&x=1"), Some("DEF".to_owned()));
        assert_eq!(parse_code("  RAW  "), Some("RAW".to_owned()));
        assert_eq!(parse_code(""), None);
    }
}
