//! Google OAuth for Gemini via Code Assist API.
//!
//! Uses the Gemini CLI OAuth flow to access Gemini models through
//! Google's Code Assist API (cloudcode-pa.googleapis.com).
//!
//! Note: This is different from the consumer Gemini API which only supports API keys.

use super::pkce::{PkceCodes, generate_state};
use super::server::CallbackServer;
use super::storage::OAuthTokens;
use super::{CALLBACK_TIMEOUT, OAuthFlow};
use anyhow::{Context, Result};
use serde::Deserialize;

/// Gemini CLI OAuth client ID.
pub const CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";

/// Gemini CLI OAuth client secret (safe for installed apps per OAuth spec).
pub const CLIENT_SECRET: &str = "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";

/// Google OAuth endpoints.
pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// OAuth scopes for Code Assist access (Gemini CLI scopes).
pub const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
                          https://www.googleapis.com/auth/userinfo.email \
                          https://www.googleapis.com/auth/userinfo.profile";

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

        // Start callback server
        let server = CallbackServer::new(state.clone())?;
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
                ("client_secret", CLIENT_SECRET),
            ])
            .send()
            .await
            .context("Failed to send refresh request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed: {} - {}", status, text);
        }

        let token_response: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

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
            anyhow::bail!("Token exchange failed: {} - {}", status, text);
        }

        let token_response: TokenResponse = response
            .json()
            .await
            .context("Failed to parse token response")?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Ok(OAuthTokens {
            access_token: token_response.access_token,
            refresh_token: token_response.refresh_token,
            expires_at: token_response.expires_in.map(|secs| now + secs * 1000),
            id_token: token_response.id_token,
        })
    }
}
