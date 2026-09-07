//! Provider authentication: /login and /logout (pi parity).
//!
//! Ion's two model providers each have a first-party flow:
//! - OpenRouter: a PKCE OAuth dance whose loopback callback yields a
//!   permanent, user-controlled API key (pi's `openRouterOAuth`).
//! - OpenAI Codex: the ChatGPT OAuth flow with a fixed 1455 loopback
//!   callback (pi's `openaiCodexOAuth`), or a device-code flow for
//!   headless sessions.
//!
//! Credentials persist to pi's shared auth file
//! (`~/.pi/agent/auth.json`, `ION_PI_AUTH` overrides the path) with
//! pi's exact JSON shape, so a credential logged in through ion is
//! the same credential pi would read — and vice versa. Writes are
//! serialized read-modify-write cycles over the whole document.

pub mod codex;
pub mod openrouter;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Login failures surfaced to the TUI as notices.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("login cancelled")]
    Cancelled,
    #[error("the callback server could not bind: {0}")]
    Bind(String),
    #[error("the token exchange failed: {0}")]
    Exchange(String),
    #[error("the authorization server returned an error: {0}")]
    Provider(String),
    #[error("login timed out")]
    Timeout,
    #[error("the auth file could not be updated: {0}")]
    Storage(String),
}

impl From<std::io::Error> for LoginError {
    fn from(err: std::io::Error) -> Self {
        Self::Storage(err.to_string())
    }
}

impl From<serde_json::Error> for LoginError {
    fn from(err: serde_json::Error) -> Self {
        Self::Storage(err.to_string())
    }
}

/// One stored credential, exactly pi's auth.json shape per provider
/// entry: `{"type":"api_key","key":...}` or
/// `{"type":"oauth","access":...,"refresh":...,"expires":ms,...}` —
/// an internally tagged enum over the `type` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Credential {
    #[serde(rename = "oauth")]
    Oauth {
        access: String,
        #[serde(default)]
        refresh: String,
        /// Epoch milliseconds; OpenRouter's permanent keys use pi's
        /// `Number.MAX_SAFE_INTEGER`.
        expires: i64,
        #[serde(default, rename = "accountId", skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
    #[serde(rename = "api_key")]
    ApiKey { key: String },
}

/// The shared authentication document. Reads and writes go through
/// `modify`, which re-reads the file inside a best-effort lock file
/// so concurrent writers (pi itself) do not clobber each other.
#[derive(Debug, Default)]
pub struct AuthFile {
    path: PathBuf,
}

/// Providers ion can log into, with display metadata for the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginProvider {
    pub id: String,
    pub name: String,
    pub methods: Vec<LoginMethod>,
}

/// One selectable login method for a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginMethod {
    pub id: String,
    pub label: String,
}

impl AuthFile {
    /// The default shared file pi owns (`~/.pi/agent/auth.json`).
    pub fn shared() -> Self {
        Self {
            path: default_auth_path(),
        }
    }

    pub fn with_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    fn read_document(&self) -> Result<serde_json::Map<String, serde_json::Value>, LoginError> {
        if !self.path.exists() {
            return Ok(serde_json::Map::new());
        }
        let text = std::fs::read_to_string(&self.path)?;
        if text.trim().is_empty() {
            return Ok(serde_json::Map::new());
        }
        let value: serde_json::Value = serde_json::from_str(&text)?;
        Ok(value
            .as_object()
            .cloned()
            .unwrap_or_else(serde_json::Map::new))
    }

    /// Read one provider's credential, if stored.
    pub fn read(&self, provider: &str) -> Result<Option<Credential>, LoginError> {
        let document = self.read_document()?;
        let Some(value) = document.get(provider) else {
            return Ok(None);
        };
        let credential: Credential = serde_json::from_value(value.clone())
            .map_err(|err| LoginError::Storage(format!("credential for {provider}: {err}")))?;
        Ok(Some(credential))
    }

    /// List stored provider ids with their credential kind (`/logout`
    /// picker rows, pi's listCredentials).
    pub fn list(&self) -> Result<Vec<(String, &'static str)>, LoginError> {
        let document = self.read_document()?;
        Ok(document
            .into_iter()
            .filter_map(|(provider, value)| {
                let kind = value.get("type")?.as_str()?;
                let kind = match kind {
                    "oauth" => "oauth",
                    "api_key" => "api_key",
                    _ => return None,
                };
                Some((provider, kind))
            })
            .collect())
    }

    /// Store one provider's credential (a serialized read-modify-write
    /// over the whole document). A `None` credential removes the
    /// entry (/logout).
    pub fn write(&self, provider: &str, credential: Option<&Credential>) -> Result<(), LoginError> {
        let lock = simple_file_lock(&self.path)?;
        let mut document = self.read_document()?;
        match credential {
            Some(credential) => {
                let value = serde_json::to_value(credential)?;
                document.insert(provider.to_owned(), value);
            }
            None => {
                document.remove(provider);
            }
        }
        let text = serde_json::to_string_pretty(&document)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, text)?;
        drop(lock);
        Ok(())
    }

    /// The OpenRouter key resolution order (pi's resolve: stored
    /// credential first, then the environment).
    pub fn openrouter_key(&self) -> Result<Option<String>, LoginError> {
        if let Some(Credential::Oauth { access, .. }) = self.read("openrouter")? {
            return Ok(Some(access));
        }
        if let Some(Credential::ApiKey { key, .. }) = self.read("openrouter")? {
            return Ok(Some(key));
        }
        Ok(std::env::var("OPENROUTER_API_KEY").ok())
    }
}

/// The default shared auth path, mirroring ion's existing pi-auth
/// reading (`openai_codex::pi_auth_path`) so both see one file.
fn default_auth_path() -> PathBuf {
    if let Some(path) = std::env::var_os("ION_PI_AUTH") {
        return path.into();
    }
    let Ok(base) = etcetera::base_strategy::choose_base_strategy() else {
        return PathBuf::from(".pi/agent/auth.json");
    };
    use etcetera::base_strategy::BaseStrategy;
    base.home_dir().join(".pi").join("agent").join("auth.json")
}

/// A minimal advisory lock: create-with-excl retries for a bounded
/// window, released by removal on drop. Keeps the write cycle short
/// so a stale lock self-heals.
struct SimpleLock(std::path::PathBuf);

impl Drop for SimpleLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn simple_file_lock(target: &std::path::Path) -> Result<SimpleLock, LoginError> {
    let lock_path = target.with_extension("ionlock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => return Ok(SimpleLock(lock_path)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::time::Instant::now() > deadline {
                    return Err(LoginError::Storage(format!(
                        "auth file lock {} is held; timed out",
                        lock_path.display()
                    )));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(err) => return Err(LoginError::Storage(err.to_string())),
        }
    }
}

// Browser launches are deliberately out of the login flows: ion
// displays the authorize URL and the user opens it. Auto-launching a
// browser from a terminal agent surprised the user twice (2026-09-06)
// and buys nothing the displayed URL does not.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_roundtrips_pi_shapes() {
        let oauth = serde_json::json!({
            "type": "oauth",
            "access": "key-1",
            "refresh": "",
            "expires": 9007199254740991i64
        });
        let credential: Credential = serde_json::from_value(oauth).expect("oauth parses");
        assert_eq!(
            credential,
            Credential::Oauth {
                access: "key-1".to_owned(),
                refresh: String::new(),
                expires: 9_007_199_254_740_991,
                account_id: None
            }
        );
        let api_key = serde_json::json!({"type": "api_key", "key": "sk-abc"});
        let credential: Credential =
            serde_json::from_value(api_key.clone()).expect("api_key parses");
        assert_eq!(
            credential,
            Credential::ApiKey {
                key: "sk-abc".to_owned()
            }
        );
        // The tag round-trips: pi reads exactly what ion writes.
        let back = serde_json::to_value(&credential).expect("api_key serializes");
        assert_eq!(back, api_key);
    }

    #[test]
    fn auth_file_write_read_remove_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = AuthFile::with_path(dir.path().join("auth.json"));
        assert!(file.read("openrouter").expect("read empty").is_none());

        let credential = Credential::Oauth {
            access: "or-key".to_owned(),
            refresh: String::new(),
            expires: 9_007_199_254_740_991,
            account_id: None,
        };
        file.write("openrouter", Some(&credential)).expect("write");
        assert_eq!(
            file.openrouter_key().expect("key"),
            Some("or-key".to_owned())
        );
        assert_eq!(
            file.list().expect("list"),
            vec![("openrouter".to_owned(), "oauth")]
        );

        // Removal deletes the entry; the environment (unset here in
        // test runs) remains pi's fallback order after stored keys.
        file.write("openrouter", None).expect("remove");
        assert!(file.read("openrouter").expect("read removed").is_none());
    }
}
