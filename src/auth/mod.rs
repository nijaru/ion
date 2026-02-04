//! OAuth authentication for subscription-based providers.
//!
//! Supports `ChatGPT` Plus/Pro (`OpenAI` OAuth) and Google AI (Google OAuth)
//! for using consumer subscriptions instead of API credits.

mod pkce;
mod server;
mod storage;

pub mod google;
pub mod openai;

pub use pkce::PkceCodes;
pub use server::{CallbackResult, CallbackServer};
pub use storage::{AuthStorage, Credentials, OAuthTokens};

use anyhow::Result;
use std::time::Duration;

/// Supported OAuth providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthProvider {
    /// `OpenAI` OAuth for `ChatGPT` Plus/Pro subscriptions.
    OpenAI,
    /// Google OAuth for Google AI subscriptions.
    Google,
}

impl OAuthProvider {
    /// Storage key for this provider.
    #[must_use]
    pub fn storage_key(&self) -> &'static str {
        match self {
            Self::OpenAI => "openai",
            Self::Google => "google",
        }
    }

    /// Human-readable name.
    #[must_use]
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::OpenAI => "ChatGPT Plus/Pro",
            Self::Google => "Google AI",
        }
    }
}

/// Common trait for OAuth login flows.
pub trait OAuthFlow {
    /// Perform the OAuth login flow.
    fn login(&self) -> impl std::future::Future<Output = Result<OAuthTokens>> + Send;

    /// Refresh an expired access token.
    fn refresh(
        &self,
        refresh_token: &str,
    ) -> impl std::future::Future<Output = Result<OAuthTokens>> + Send;
}

/// Login to an OAuth provider.
pub async fn login(provider: OAuthProvider) -> Result<()> {
    let storage = AuthStorage::new()?;

    let tokens = match provider {
        OAuthProvider::OpenAI => openai::OpenAIAuth::new().login().await?,
        OAuthProvider::Google => google::GoogleAuth::new().login().await?,
    };

    storage.save(provider, Credentials::OAuth(tokens))?;
    Ok(())
}

/// Logout from an OAuth provider.
pub fn logout(provider: OAuthProvider) -> Result<()> {
    let storage = AuthStorage::new()?;
    storage.clear(provider)?;
    Ok(())
}

/// Get valid credentials for a provider, refreshing if needed.
pub async fn get_credentials(provider: OAuthProvider) -> Result<Option<Credentials>> {
    let storage = AuthStorage::new()?;

    let Some(creds) = storage.load(provider)? else {
        return Ok(None);
    };

    // Fill in ChatGPT account ID if missing and id_token is present.
    if provider == OAuthProvider::OpenAI {
        if let Credentials::OAuth(ref tokens) = creds
            && tokens.chatgpt_account_id.is_none()
            && let Some(id_token) = tokens.id_token.as_deref()
            && let Some(account_id) = openai::extract_chatgpt_account_id(id_token)
        {
            let mut updated = tokens.clone();
            updated.chatgpt_account_id = Some(account_id);
            storage.save(provider, Credentials::OAuth(updated.clone()))?;
            return Ok(Some(Credentials::OAuth(updated)));
        }
    }

    // Check if OAuth tokens need refresh
    if let Credentials::OAuth(ref tokens) = creds
        && tokens.needs_refresh()
    {
        match &tokens.refresh_token {
            Some(refresh_token) => {
                let mut new_tokens = match provider {
                    OAuthProvider::OpenAI => {
                        openai::OpenAIAuth::new().refresh(refresh_token).await?
                    }
                    OAuthProvider::Google => {
                        google::GoogleAuth::new().refresh(refresh_token).await?
                    }
                };
                // Preserve id_token/account id if refresh doesn't return them.
                if new_tokens.id_token.is_none() {
                    new_tokens.id_token = tokens.id_token.clone();
                }
                if new_tokens.chatgpt_account_id.is_none() {
                    new_tokens.chatgpt_account_id = tokens.chatgpt_account_id.clone();
                }
                if provider == OAuthProvider::OpenAI
                    && new_tokens.chatgpt_account_id.is_none()
                    && let Some(id_token) = new_tokens.id_token.as_deref()
                {
                    new_tokens.chatgpt_account_id =
                        openai::extract_chatgpt_account_id(id_token);
                }
                storage.save(provider, Credentials::OAuth(new_tokens.clone()))?;
                return Ok(Some(Credentials::OAuth(new_tokens)));
            }
            None => {
                // Token expired and no refresh token available
                anyhow::bail!(
                    "OAuth token expired. Please run 'ion login {}' again.",
                    provider.storage_key()
                );
            }
        }
    }

    Ok(Some(creds))
}

/// Check if a provider has usable credentials (not expired, or can be refreshed).
#[must_use]
pub fn is_logged_in(provider: OAuthProvider) -> bool {
    AuthStorage::new()
        .ok()
        .and_then(|s| s.load(provider).ok().flatten())
        .is_some_and(|c| c.is_usable())
}

/// Default callback timeout.
pub const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes
