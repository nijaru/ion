//! `OpenAI` OAuth for `ChatGPT` Plus/Pro subscriptions.
//!
//! Uses the same OAuth flow as Codex CLI to authenticate with
//! `ChatGPT` Plus ($20/month) or `ChatGPT` Pro ($200/month) subscriptions.

use super::pkce::{PkceCodes, generate_state};
use super::server::CallbackServer;
use super::storage::OAuthTokens;
use super::{CALLBACK_TIMEOUT, OAuthFlow};
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::Value;

/// `OpenAI` OAuth client ID (same as Codex CLI).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// `OpenAI` OAuth endpoints.
pub const AUTH_ENDPOINT: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";

/// OAuth scopes for `ChatGPT` access.
pub const SCOPES: &str = "openid profile email offline_access";
/// Include org/workspace IDs in id_token.
pub const ID_TOKEN_ADD_ORGANIZATIONS: &str = "true";
/// Use Codex simplified flow (avoids API org/project selection).
pub const CODEX_CLI_SIMPLIFIED_FLOW: &str = "true";
/// Identify the client to OpenAI (match Codex CLI).
pub const ORIGINATOR: &str = "codex_cli_rs";

/// `OpenAI` OAuth authentication.
pub struct OpenAIAuth {
    client: reqwest::Client,
}

impl OpenAIAuth {
    /// Create a new `OpenAI` auth handler.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("Failed to build HTTP client"),
        }
    }

    #[allow(clippy::unused_self)]
    fn build_auth_url(&self, redirect_uri: &str, state: &str, pkce: &PkceCodes) -> String {
        format!(
            "{}?response_type=code\
             &client_id={}\
             &redirect_uri={}\
             &scope={}\
             &state={}\
             &code_challenge={}\
             &code_challenge_method=S256\
             &id_token_add_organizations={}\
             &codex_cli_simplified_flow={}\
             &originator={}",
            AUTH_ENDPOINT,
            CLIENT_ID,
            urlencoding::encode(redirect_uri),
            urlencoding::encode(SCOPES),
            state,
            pkce.challenge,
            ID_TOKEN_ADD_ORGANIZATIONS,
            CODEX_CLI_SIMPLIFIED_FLOW,
            urlencoding::encode(ORIGINATOR)
        )
    }
}

impl Default for OpenAIAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthFlow for OpenAIAuth {
    async fn login(&self) -> Result<OAuthTokens> {
        // Generate PKCE codes and state
        let pkce = PkceCodes::generate();
        let state = generate_state();

        // Start callback server
        let server = CallbackServer::new(state.clone())?;
        let redirect_uri = server.redirect_uri();
        let port = server.port();

        // Build authorization URL
        let auth_url = self.build_auth_url(&redirect_uri, &state, &pkce);

        // Print instructions
        println!("Opening browser for ChatGPT login...");
        println!("If the browser doesn't open, visit:");
        println!("  {auth_url}");
        println!();

        // Open browser
        if let Err(e) = open::that(&auth_url) {
            eprintln!("Failed to open browser: {e}");
            eprintln!("Please open the URL above manually.");
        }

        // Wait for callback
        println!("Waiting for authentication on http://127.0.0.1:{port}...");
        let callback = server.wait_for_callback(CALLBACK_TIMEOUT)?;

        // Exchange code for tokens
        println!("Exchanging authorization code...");
        let tokens = self
            .exchange_code(&callback.code, &redirect_uri, &pkce)
            .await?;

        println!("Login successful!");
        Ok(tokens)
    }

    async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokens> {
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            refresh_token: Option<String>,
            expires_in: Option<u64>,
            id_token: Option<String>,
        }

        let response = self
            .client
            .post(TOKEN_ENDPOINT)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .await
            .context("Failed to send refresh request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed: {status} - {text}");
        }

        let token_response: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

        #[allow(clippy::cast_possible_truncation)] // ms since epoch won't overflow u64
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let id_token = token_response.id_token;
        let chatgpt_account_id = id_token
            .as_deref()
            .and_then(extract_chatgpt_account_id);

        Ok(OAuthTokens {
            access_token: token_response.access_token,
            refresh_token: token_response
                .refresh_token
                .or_else(|| Some(refresh_token.to_string())),
            expires_at: token_response.expires_in.map(|secs| now + secs * 1000),
            id_token,
            chatgpt_account_id,
            google_project_id: None,
        })
    }
}

impl OpenAIAuth {
    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        pkce: &PkceCodes,
    ) -> Result<OAuthTokens> {
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            refresh_token: Option<String>,
            expires_in: Option<u64>,
            id_token: Option<String>,
        }

        let response = self
            .client
            .post(TOKEN_ENDPOINT)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("client_id", CLIENT_ID),
                ("code_verifier", &pkce.verifier),
            ])
            .send()
            .await
            .context("Failed to send token request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Token exchange failed: {status} - {text}");
        }

        let token_response: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

        #[allow(clippy::cast_possible_truncation)] // ms since epoch won't overflow u64
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let id_token = token_response.id_token;
        let chatgpt_account_id = id_token
            .as_deref()
            .and_then(extract_chatgpt_account_id);

        Ok(OAuthTokens {
            access_token: token_response.access_token,
            refresh_token: token_response.refresh_token,
            expires_at: token_response.expires_in.map(|secs| now + secs * 1000),
            id_token,
            chatgpt_account_id,
            google_project_id: None,
        })
    }
}

pub(crate) fn extract_chatgpt_account_id(id_token: &str) -> Option<String> {
    let payload_b64 = id_token.split('.').nth(1)?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let claims: Value = serde_json::from_slice(&payload_bytes).ok()?;
    claims
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::pkce::PkceCodes;

    #[test]
    fn test_auth_url_contains_required_params() {
        let auth = OpenAIAuth::new();
        let pkce = PkceCodes::generate();
        let url = auth.build_auth_url("http://localhost:8080/callback", "test_state", &pkce);

        assert!(url.starts_with(AUTH_ENDPOINT));
        assert!(url.contains("response_type=code"));
        assert!(url.contains(&format!("client_id={CLIENT_ID}")));
        assert!(url.contains("redirect_uri="));
        assert!(url.contains("state=test_state"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[test]
    fn test_auth_url_encodes_redirect_uri() {
        let auth = OpenAIAuth::new();
        let pkce = PkceCodes::generate();
        let url = auth.build_auth_url("http://localhost:8080/callback", "state", &pkce);

        // The redirect URI should be URL-encoded
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fcallback"));
    }

    #[test]
    fn test_auth_url_includes_scopes() {
        let auth = OpenAIAuth::new();
        let pkce = PkceCodes::generate();
        let url = auth.build_auth_url("http://localhost:8080", "state", &pkce);

        // Scopes should be URL-encoded
        assert!(url.contains("scope="));
        assert!(url.contains("openid"));
    }

    #[test]
    fn test_auth_url_includes_codex_params() {
        let auth = OpenAIAuth::new();
        let pkce = PkceCodes::generate();
        let url = auth.build_auth_url("http://localhost:8080", "state", &pkce);

        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains(&format!("originator={ORIGINATOR}")));
    }

    #[test]
    fn test_extract_chatgpt_account_id() {
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_123"
            }
        });
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload_bytes);
        let token = format!("header.{payload_b64}.sig");

        assert_eq!(
            extract_chatgpt_account_id(&token),
            Some("acct_123".to_string())
        );
    }

    #[test]
    fn test_client_id_matches_codex() {
        // Verify we're using the same client ID as Codex CLI
        assert_eq!(CLIENT_ID, "app_EMoamEEZ73f0CkXaXp7hrann");
    }

    #[test]
    fn test_endpoints_are_valid() {
        assert!(AUTH_ENDPOINT.starts_with("https://"));
        assert!(TOKEN_ENDPOINT.starts_with("https://"));
        assert!(AUTH_ENDPOINT.contains("openai.com"));
        assert!(TOKEN_ENDPOINT.contains("openai.com"));
    }
}
