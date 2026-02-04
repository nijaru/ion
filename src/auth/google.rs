//! Google OAuth for Gemini subscriptions via Antigravity.
//!
//! Uses the Antigravity OAuth flow to access Gemini models through
//! the Code Assist backend (cloudcode-pa.googleapis.com).
//! This enables access using Google AI Pro/Ultra subscriptions.

use super::pkce::{PkceCodes, generate_state};
use super::server::CallbackServer;
use super::storage::OAuthTokens;
use super::{CALLBACK_TIMEOUT, OAuthFlow};
use anyhow::{Context, Result};
use serde::Deserialize;

/// Antigravity OAuth client ID.
pub const CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";

/// Antigravity OAuth client secret (safe for installed apps per OAuth spec).
pub const CLIENT_SECRET: &str = "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";

/// Google OAuth endpoints.
pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// OAuth scopes for Antigravity access.
pub const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
                          https://www.googleapis.com/auth/userinfo.email \
                          https://www.googleapis.com/auth/userinfo.profile \
                          https://www.googleapis.com/auth/cclog \
                          https://www.googleapis.com/auth/experimentsandconfigs";

/// Antigravity OAuth callback path and port (fixed).
const CALLBACK_PATH: &str = "/oauth-callback";
const CALLBACK_PORT: u16 = 51121;

/// Default project ID used when loadCodeAssist does not return one.
const DEFAULT_PROJECT_ID: &str = "rising-fact-p41fc";

/// Code Assist endpoints used to resolve the project ID.
const LOAD_ENDPOINTS: &[&str] = &[
    "https://cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
    "https://autopush-cloudcode-pa.sandbox.googleapis.com",
];

const CLIENT_METADATA: &str =
    r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#;
const GEMINI_CLI_USER_AGENT: &str = "google-api-nodejs-client/10.3.0";
const GEMINI_CLI_API_CLIENT: &str = "gl-node/22.18.0";

/// Google OAuth authentication.
pub struct GoogleAuth {
    client: reqwest::Client,
}

impl GoogleAuth {
    /// Create a new Google auth handler.
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
             &access_type=offline\
             &prompt=consent",
            AUTH_ENDPOINT,
            CLIENT_ID,
            urlencoding::encode(redirect_uri),
            urlencoding::encode(SCOPES),
            state,
            pkce.challenge
        )
    }
}

impl Default for GoogleAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthFlow for GoogleAuth {
    async fn login(&self) -> Result<OAuthTokens> {
        // Generate PKCE codes and state
        let pkce = PkceCodes::generate();
        let state = generate_state();

        // Start callback server (fixed port/path for Antigravity)
        let server = CallbackServer::new_with_config(
            state.clone(),
            super::server::CallbackConfig::fixed(CALLBACK_PORT, CALLBACK_PATH),
        )?;
        let redirect_uri = server.redirect_uri();
        let port = server.port();

        // Build authorization URL
        let auth_url = self.build_auth_url(&redirect_uri, &state, &pkce);

        // Print instructions
        println!("Opening browser for Google AI login...");
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
        let mut tokens = self
            .exchange_code(&callback.code, &redirect_uri, &pkce)
            .await?;
        tokens.google_project_id = Some(self.resolve_project_id(&tokens.access_token).await?);

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
                ("client_secret", CLIENT_SECRET),
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

        Ok(OAuthTokens {
            access_token: token_response.access_token,
            refresh_token: token_response
                .refresh_token
                .or_else(|| Some(refresh_token.to_string())),
            expires_at: token_response.expires_in.map(|secs| now + secs * 1000),
            id_token: token_response.id_token,
            chatgpt_account_id: None,
            google_project_id: None,
        })
    }
}

impl GoogleAuth {
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
                ("client_secret", CLIENT_SECRET),
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

        Ok(OAuthTokens {
            access_token: token_response.access_token,
            refresh_token: token_response.refresh_token,
            expires_at: token_response.expires_in.map(|secs| now + secs * 1000),
            id_token: token_response.id_token,
            chatgpt_account_id: None,
            google_project_id: None,
        })
    }

    pub(crate) async fn resolve_project_id(&self, access_token: &str) -> Result<String> {
        let mut errors = Vec::new();
        let metadata = serde_json::json!({
            "metadata": {
                "ideType": "IDE_UNSPECIFIED",
                "platform": "PLATFORM_UNSPECIFIED",
                "pluginType": "GEMINI",
            }
        });

        for endpoint in LOAD_ENDPOINTS {
            let url = format!("{endpoint}/v1internal:loadCodeAssist");
            let response = self
                .client
                .post(&url)
                .headers(self.build_load_headers(access_token))
                .json(&metadata)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await;

            match response {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        let text = resp.text().await.unwrap_or_default();
                        errors.push(format!("{endpoint} {status}: {text}"));
                        continue;
                    }

                    let value: serde_json::Value = resp
                        .json()
                        .await
                        .context("Failed to parse loadCodeAssist response")?;

                    if let Some(project_id) = extract_project_id(&value) {
                        return Ok(project_id);
                    }

                    errors.push(format!("{endpoint} missing project id"));
                }
                Err(err) => {
                    errors.push(format!("{endpoint} error: {err}"));
                }
            }
        }

        if !errors.is_empty() {
            tracing::warn!(
                "loadCodeAssist failed, falling back to default project id: {}",
                errors.join("; ")
            );
        }

        Ok(DEFAULT_PROJECT_ID.to_string())
    }

    fn build_load_headers(&self, access_token: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {access_token}").parse().unwrap(),
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        headers.insert(reqwest::header::USER_AGENT, GEMINI_CLI_USER_AGENT.parse().unwrap());
        headers.insert(
            reqwest::header::HeaderName::from_static("x-goog-api-client"),
            GEMINI_CLI_API_CLIENT.parse().unwrap(),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("client-metadata"),
            CLIENT_METADATA.parse().unwrap(),
        );
        headers
    }
}

fn extract_project_id(value: &serde_json::Value) -> Option<String> {
    let project = value.get("cloudaicompanionProject")?;
    if let Some(project_id) = project.as_str() {
        return Some(project_id.to_string());
    }
    project
        .get("id")
        .and_then(|id| id.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::pkce::PkceCodes;

    #[test]
    fn test_auth_url_contains_required_params() {
        let auth = GoogleAuth::new();
        let pkce = PkceCodes::generate();
        let url = auth.build_auth_url("http://localhost:8080/callback", "test_state", &pkce);

        assert!(url.starts_with(AUTH_ENDPOINT));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id="));
        assert!(url.contains("redirect_uri="));
        assert!(url.contains("state=test_state"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[test]
    fn test_auth_url_has_offline_access() {
        let auth = GoogleAuth::new();
        let pkce = PkceCodes::generate();
        let url = auth.build_auth_url("http://localhost:8080", "state", &pkce);

        // Google requires access_type=offline for refresh tokens
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
    }

    #[test]
    fn test_auth_url_includes_cloud_platform_scope() {
        let auth = GoogleAuth::new();
        let pkce = PkceCodes::generate();
        let url = auth.build_auth_url("http://localhost:8080", "state", &pkce);

        // Cloud platform scope is required for Code Assist API
        assert!(url.contains("cloud-platform"));
    }

    #[test]
    fn test_client_id_matches_antigravity() {
        // Verify we're using the Antigravity client ID
        assert!(CLIENT_ID.ends_with(".apps.googleusercontent.com"));
        assert!(CLIENT_ID.starts_with("1071006060591"));
    }

    #[test]
    fn test_endpoints_are_valid() {
        assert!(AUTH_ENDPOINT.starts_with("https://"));
        assert!(TOKEN_ENDPOINT.starts_with("https://"));
        assert!(AUTH_ENDPOINT.contains("google.com"));
        assert!(TOKEN_ENDPOINT.contains("googleapis.com"));
    }

    #[test]
    fn test_scopes_include_user_info() {
        // User info scopes are needed for identification
        assert!(SCOPES.contains("userinfo.email"));
        assert!(SCOPES.contains("userinfo.profile"));
    }

    #[test]
    fn test_scopes_include_antigravity_internal() {
        assert!(SCOPES.contains("cclog"));
        assert!(SCOPES.contains("experimentsandconfigs"));
    }
}
