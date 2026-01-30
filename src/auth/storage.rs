//! Credential storage for OAuth tokens.

use super::OAuthProvider;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// OAuth token set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    /// Access token for API calls.
    pub access_token: String,
    /// Refresh token for getting new access tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Token expiration timestamp (milliseconds since epoch).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// ID token (OpenID Connect).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
}

impl OAuthTokens {
    /// Check if the access token needs refresh.
    ///
    /// Returns true if token expires within 5 minutes.
    #[must_use]
    pub fn needs_refresh(&self) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let refresh_threshold = 5 * 60 * 1000; // 5 minutes
        expires_at.saturating_sub(now) < refresh_threshold
    }

    /// Check if the access token is expired.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        now >= expires_at
    }
}

/// Stored credentials (API key or OAuth tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credentials {
    /// API key authentication.
    ApiKey { key: String },
    /// OAuth token authentication.
    #[serde(rename = "oauth")]
    OAuth(OAuthTokens),
}

impl Credentials {
    /// Get the access token (for OAuth) or API key.
    #[must_use]
    pub fn token(&self) -> &str {
        match self {
            Self::ApiKey { key } => key,
            Self::OAuth(tokens) => &tokens.access_token,
        }
    }
}

/// Storage file format.
#[derive(Debug, Default, Serialize, Deserialize)]
struct AuthFile {
    #[serde(flatten)]
    providers: HashMap<String, Credentials>,
}

/// Credential storage manager.
pub struct AuthStorage {
    path: PathBuf,
}

impl AuthStorage {
    /// Create a new storage manager.
    pub fn new() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .context("Could not find config directory")?
            .join("ion");

        fs::create_dir_all(&config_dir)?;

        Ok(Self {
            path: config_dir.join("auth.json"),
        })
    }

    /// Load credentials for a provider.
    pub fn load(&self, provider: OAuthProvider) -> Result<Option<Credentials>> {
        let auth_file = self.read_file()?;
        Ok(auth_file.providers.get(provider.storage_key()).cloned())
    }

    /// Save credentials for a provider.
    pub fn save(&self, provider: OAuthProvider, credentials: Credentials) -> Result<()> {
        let mut auth_file = self.read_file()?;
        auth_file
            .providers
            .insert(provider.storage_key().to_string(), credentials);
        self.write_file(&auth_file)
    }

    /// Clear credentials for a provider.
    pub fn clear(&self, provider: OAuthProvider) -> Result<()> {
        let mut auth_file = self.read_file()?;
        auth_file.providers.remove(provider.storage_key());
        self.write_file(&auth_file)
    }

    /// List all stored providers.
    pub fn list(&self) -> Result<Vec<String>> {
        let auth_file = self.read_file()?;
        Ok(auth_file.providers.keys().cloned().collect())
    }

    fn read_file(&self) -> Result<AuthFile> {
        if !self.path.exists() {
            return Ok(AuthFile::default());
        }

        let content = fs::read_to_string(&self.path)?;
        let auth_file: AuthFile = serde_json::from_str(&content)?;
        Ok(auth_file)
    }

    fn write_file(&self, auth_file: &AuthFile) -> Result<()> {
        let content = serde_json::to_string_pretty(auth_file)?;
        fs::write(&self.path, content)?;

        // Set restrictive permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&self.path, permissions)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oauth_tokens_needs_refresh() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Token expires in 1 minute - should need refresh
        let tokens = OAuthTokens {
            access_token: "test".into(),
            refresh_token: None,
            expires_at: Some(now + 60_000),
            id_token: None,
        };
        assert!(tokens.needs_refresh());

        // Token expires in 10 minutes - should not need refresh
        let tokens = OAuthTokens {
            access_token: "test".into(),
            refresh_token: None,
            expires_at: Some(now + 600_000),
            id_token: None,
        };
        assert!(!tokens.needs_refresh());
    }

    #[test]
    fn test_credentials_serialization() {
        let oauth = Credentials::OAuth(OAuthTokens {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at: Some(1234567890000),
            id_token: None,
        });

        let json = serde_json::to_string(&oauth).unwrap();
        assert!(json.contains("\"type\":\"oauth\""));
        assert!(json.contains("\"access_token\":\"access\""));
    }
}
