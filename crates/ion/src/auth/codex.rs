//! OpenAI Codex (ChatGPT) OAuth flow (pi's `auth/oauth/openai-codex.ts`).
//!
//! Browser login: PKCE + a fixed loopback callback on port 1455. The
//! token exchange returns an expiring access token plus a refresh
//! token; the account id comes from the access token's JWT claims.
//! Device-code login covers headless sessions: the user enters a code
//! at the verification URL while the flow polls.

use std::time::Duration;

use base64::Engine as _;

use super::{Credential, LoginError};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
pub const DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const SCOPE_ENCODED: &str = "openid%20profile%20email%20offline_access";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Shared PKCE generation (same shape as the OpenRouter flow).
pub(super) use super::openrouter::generate_pkce;

/// The authorize URL plus the loopback listener that completes a
/// browser login. Dropping the listener stops the server.
pub struct CodexLoginListener {
    pub authorize_url: String,
    pub redirect_uri: &'static str,
    cancel: tokio_util::sync::CancellationToken,
    code_rx: Option<tokio::sync::oneshot::Receiver<Result<String, String>>>,
    verifier: String,
    state: String,
}

impl CodexLoginListener {
    pub async fn start() -> Result<Self, LoginError> {
        let pkce = generate_pkce()?;
        let state = random_hex(16)?;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 1455))
            .await
            .map_err(|err| LoginError::Bind(format!("port 1455: {err}")))?;

        // Every fixed word is URL-safe; variable values are pre-encoded
        // (base64url challenge, hex state, %20 scope). The redirect URI
        // needs no encoding: `:` and `/` are legal query characters.
        let authorize_url = format!(
            "{AUTHORIZE_URL}?response_type=code&client_id={CLIENT_ID}\
             &redirect_uri={REDIRECT_URI}&scope={SCOPE_ENCODED}\
             &code_challenge={}&code_challenge_method=S256&state={state}\
             &id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=ion",
            pkce.challenge,
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        let (code_tx, code_rx) = tokio::sync::oneshot::channel();
        let task_cancel = cancel.clone();
        let expected_state = state.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = task_cancel.cancelled() => {}
                accepted = accept_callback(listener, &expected_state) => {
                    let _ = code_tx.send(accepted);
                }
            }
        });

        Ok(Self {
            authorize_url,
            redirect_uri: REDIRECT_URI,
            cancel,
            code_rx: Some(code_rx),
            verifier: pkce.verifier,
            state,
        })
    }

    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    pub fn state(&self) -> &str {
        &self.state
    }

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

impl Drop for CodexLoginListener {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Accept the fixed-path callback; validate the state parameter.
async fn accept_callback(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String, String> {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return Err("the callback listener stopped".to_owned());
        };
        let Ok(request) = super::openrouter::read_request_head(&mut stream).await else {
            continue;
        };
        if let Some(state) = query_param(&request, "state")
            && state != expected_state
        {
            _ = super::openrouter::respond(&mut stream, 400, "State mismatch.").await;
            return Err("state mismatch in the OAuth callback".to_owned());
        }
        if let Some(error) = query_param(&request, "error") {
            _ = super::openrouter::respond(&mut stream, 400, "Authorization failed.").await;
            return Err(format!(
                "authorization failed: {}",
                query_param(&request, "error_description").unwrap_or(error)
            ));
        }
        if !request.starts_with("GET") {
            _ = super::openrouter::respond(&mut stream, 404, "Not found.").await;
            continue;
        }
        let Some(code) = query_param(&request, "code") else {
            _ = super::openrouter::respond(&mut stream, 400, "Missing authorization code.").await;
            return Err("OpenAI returned no authorization code".to_owned());
        };
        _ = super::openrouter::respond(
            &mut stream,
            200,
            "OpenAI authentication completed. You can close this window.",
        )
        .await;
        return Ok(code);
    }
}

fn query_param(request: &str, name: &str) -> Option<String> {
    super::openrouter::query_param(request, name)
}

/// One device-code login's starting state: what the user must visit
/// and enter, plus the poll parameters.
pub struct DeviceAuth {
    pub user_code: String,
    pub verification_uri: &'static str,
    pub device_auth_id: String,
    interval: Duration,
}

/// Start a device-code login: request a user code from OpenAI.
pub async fn start_device_auth() -> Result<DeviceAuth, LoginError> {
    let client = reqwest::Client::new();
    let response = client
        .post(DEVICE_USER_CODE_URL)
        .json(&serde_json::json!({ "client_id": CLIENT_ID }))
        .send()
        .await
        .map_err(|err| LoginError::Provider(err.to_string()))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|err| LoginError::Provider(format!("invalid device code response: {err}")))?;
    if status.as_u16() == 404 {
        return Err(LoginError::Provider(
            "device code login is not enabled for this server; use browser login".to_owned(),
        ));
    }
    if !status.is_success() {
        return Err(LoginError::Provider(format!(
            "device code request failed (HTTP {status})"
        )));
    }
    let device_auth_id = body
        .get("device_auth_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            LoginError::Provider("device code response missing device_auth_id".to_owned())
        })?
        .to_owned();
    let user_code = body
        .get("user_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LoginError::Provider("device code response missing user_code".to_owned()))?
        .to_owned();
    let interval_seconds = match body.get("interval") {
        Some(value) if value.is_number() => value.as_f64().unwrap_or(5.0),
        Some(value) if value.is_string() => value
            .as_str()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(5.0),
        _ => 5.0,
    };
    Ok(DeviceAuth {
        user_code,
        verification_uri: DEVICE_VERIFICATION_URI,
        device_auth_id,
        interval: Duration::from_secs_f64(interval_seconds.max(1.0)),
    })
}

/// The completed device flow: an authorization code plus its verifier.
pub struct DeviceCode {
    pub authorization_code: String,
    pub code_verifier: String,
}

/// Poll until the user authorizes, honoring `interval` and slow_down
/// (+5s per RFC 8628). Cancel by dropping the future's task.
pub async fn poll_device_auth(
    device: DeviceAuth,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<DeviceCode, LoginError> {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + LOGIN_TIMEOUT;
    let mut interval = device.interval;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Err(LoginError::Cancelled),
            _ = tokio::time::sleep(interval) => {}
        }
        if std::time::Instant::now() > deadline {
            return Err(LoginError::Timeout);
        }
        let response = client
            .post(DEVICE_TOKEN_URL)
            .json(&serde_json::json!({
                "device_auth_id": device.device_auth_id,
                "user_code": device.user_code,
            }))
            .send()
            .await
            .map_err(|err| LoginError::Provider(err.to_string()))?;
        let status = response.status().as_u16();
        let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
        match status {
            200 => {
                let code = body
                    .get("authorization_code")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        LoginError::Provider(
                            "device token response missing authorization_code".to_owned(),
                        )
                    })?
                    .to_owned();
                let verifier = body
                    .get("code_verifier")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        LoginError::Provider(
                            "device token response missing code_verifier".to_owned(),
                        )
                    })?
                    .to_owned();
                return Ok(DeviceCode {
                    authorization_code: code,
                    code_verifier: verifier,
                });
            }
            403 | 404 => {}
            _ => {
                // slow_down widens the interval; anything else fails.
                let error_code = body
                    .get("error")
                    .and_then(|e| {
                        e.as_str()
                            .map(str::to_owned)
                            .or_else(|| e.get("code").and_then(|c| c.as_str()).map(str::to_owned))
                    })
                    .unwrap_or_default();
                if error_code == "slow_down" {
                    interval += Duration::from_secs(5);
                } else if error_code == "deviceauth_authorization_pending" {
                    // pending; keep polling
                } else {
                    return Err(LoginError::Provider(format!(
                        "device auth failed (HTTP {status}): {error_code}"
                    )));
                }
            }
        }
    }
}

/// Complete a device-code login: exchange the returned authorization
/// code against the device redirect URI for the stored credential.
pub async fn complete_device_auth(device: DeviceCode) -> Result<Credential, LoginError> {
    exchange_code(
        &device.authorization_code,
        &device.code_verifier,
        DEVICE_REDIRECT_URI,
    )
    .await
}

/// Exchange the authorization code for tokens and build the stored
/// credential (pi's exchangeAuthorizationCodeForCredentials).
pub async fn exchange_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Credential, LoginError> {
    let token = token_request(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ])
    .await?;
    credential_from_token(token)
}

/// Refresh an expired access token (pi's refreshAccessToken).
pub async fn refresh(refresh_token: &str) -> Result<Credential, LoginError> {
    let token = token_request(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CLIENT_ID),
    ])
    .await?;
    credential_from_token(token)
}

struct OAuthToken {
    access: String,
    refresh: String,
    /// Epoch milliseconds.
    expires: i64,
}

async fn token_request(params: &[(&str, &str)]) -> Result<OAuthToken, LoginError> {
    let client = reqwest::Client::new();
    let response = client
        .post(TOKEN_URL)
        .form(params)
        .send()
        .await
        .map_err(|err| LoginError::Exchange(err.to_string()))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|err| LoginError::Exchange(format!("invalid token response: {err}")))?;
    if !status.is_success() {
        return Err(LoginError::Exchange(format!(
            "token request failed (HTTP {status}): {}",
            body.get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("no detail")
        )));
    }
    let access = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| LoginError::Exchange("token response missing access_token".to_owned()))?;
    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| LoginError::Exchange("token response missing refresh_token".to_owned()))?;
    let expires_in = body
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| LoginError::Exchange("token response missing expires_in".to_owned()))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default();
    Ok(OAuthToken {
        access: access.to_owned(),
        refresh: refresh_token.to_owned(),
        expires: now + expires_in * 1000,
    })
}

/// The ChatGPT account id from the access token's JWT claims (pi's
/// getAccountId via the `https://api.openai.com/auth` claim).
fn account_id_from_jwt(access_token: &str) -> Option<String> {
    let payload = access_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let account_id = value
        .get(JWT_CLAIM_PATH)?
        .get("chatgpt_account_id")?
        .as_str()?;
    (!account_id.is_empty()).then(|| account_id.to_owned())
}

fn credential_from_token(token: OAuthToken) -> Result<Credential, LoginError> {
    let account_id = account_id_from_jwt(&token.access).ok_or_else(|| {
        LoginError::Exchange("could not extract the ChatGPT account id from the token".to_owned())
    })?;
    Ok(Credential::Oauth {
        access: token.access,
        refresh: token.refresh,
        expires: token.expires,
        account_id: Some(account_id),
    })
}

fn random_hex(bytes: usize) -> Result<String, LoginError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf)
        .map_err(|err| LoginError::Exchange(format!("system randomness: {err}")))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_account_id_from_a_jwt() {
        use base64::Engine as _;
        let payload = serde_json::json!({
            JWT_CLAIM_PATH: { "chatgpt_account_id": "acct-123" }
        });
        let encoded =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        let jwt = format!("header.{encoded}.signature");
        assert_eq!(account_id_from_jwt(&jwt), Some("acct-123".to_owned()));
        assert_eq!(account_id_from_jwt("not-a-jwt"), None);
    }
}
