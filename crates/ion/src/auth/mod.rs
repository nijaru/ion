//! Provider authentication: /login and /logout (pi parity).
//!
//! Ion's two model providers each have a first-party flow:
//! - OpenRouter: a PKCE OAuth dance whose loopback callback yields a
//!   permanent, user-controlled API key (pi's `openRouterOAuth`).
//! - OpenAI Codex: the ChatGPT OAuth flow with a fixed 1455 loopback
//!   callback (pi's `openaiCodexOAuth`), or a device-code flow for
//!   headless sessions.
//!
//! Credentials belong to Ion's configuration directory. Pi credentials may be
//! read explicitly by the Codex adapter, but this store never writes them.
//! Updates serialize under an OS lock and atomically replace a private file.

pub mod codex;
pub mod openrouter;

use std::io::Write;
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

/// Ion-owned credential persistence.
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
    /// Ion's writable store; ION_PI_AUTH is never a write destination.
    pub fn owned() -> Result<Self, LoginError> {
        Ok(Self {
            path: default_auth_path()?,
        })
    }

    pub fn with_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    fn read_document(&self) -> Result<serde_json::Map<String, serde_json::Value>, LoginError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(serde_json::Map::new());
            }
            Err(err) => return Err(err.into()),
        };
        Ok(serde_json::from_str(&text)?)
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
        let lock = file_lock(&self.path)?;
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
        let text = serde_json::to_vec_pretty(&document)?;
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        // Same-directory rename publishes a complete document. NamedTempFile
        // creates mode 0600 on Unix, including when replacing a permissive file.
        let mut pending = tempfile::NamedTempFile::new_in(parent)?;
        pending.write_all(&text)?;
        pending.as_file().sync_all()?;
        pending
            .persist(&self.path)
            .map_err(|err| LoginError::Storage(err.to_string()))?;
        std::fs::File::open(parent)?.sync_all()?;
        drop(lock);
        Ok(())
    }

    /// The OpenRouter key resolution order: Ion-stored credential,
    /// then the explicitly configured environment key.
    pub fn openrouter_key(&self) -> Result<Option<String>, LoginError> {
        Ok(match self.read("openrouter")? {
            Some(Credential::Oauth { access, .. }) => Some(access),
            Some(Credential::ApiKey { key }) => Some(key),
            None => std::env::var("OPENROUTER_API_KEY").ok(),
        })
    }
}

fn default_auth_path() -> Result<PathBuf, LoginError> {
    if let Some(path) = std::env::var_os("ION_AUTH_FILE") {
        return Ok(path.into());
    }
    let base = etcetera::base_strategy::choose_base_strategy().map_err(|err| {
        LoginError::Storage(format!("cannot resolve configuration directory: {err}"))
    })?;
    use etcetera::base_strategy::BaseStrategy;
    Ok(base.config_dir().join("ion").join("auth.json"))
}

// Keep the lock inode stable across atomic replacements. OS ownership is
// released on process loss; a leftover lock file is harmless.
fn file_lock(target: &std::path::Path) -> Result<std::fs::File, LoginError> {
    let lock_path = target.with_extension("lock");
    if let Some(parent) = lock_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(&lock_path)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(err) => {
                return Err(LoginError::Storage(format!(
                    "cannot lock auth store: {err}"
                )));
            }
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
    fn rejects_corruption_without_replacing_original() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = AuthFile::with_path(dir.path().join("auth.json"));
        for invalid in ["", "null", "[]", "{broken"] {
            std::fs::write(file.path(), invalid).expect("fixture");
            assert!(file.write("openrouter", None).is_err());
            assert_eq!(
                std::fs::read_to_string(file.path()).expect("original"),
                invalid
            );
        }
    }

    #[test]
    fn concurrent_updates_preserve_each_provider_and_ignore_stale_lock_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        std::fs::write(path.with_extension("lock"), "stale").expect("stale lock");
        std::thread::scope(|scope| {
            for index in 0..12 {
                let path = path.clone();
                scope.spawn(move || {
                    AuthFile::with_path(path)
                        .write(
                            &format!("provider-{index}"),
                            Some(&Credential::ApiKey { key: "test".into() }),
                        )
                        .expect("serialized write");
                });
            }
        });
        assert_eq!(
            AuthFile::with_path(path).list().expect("all entries").len(),
            12
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacement_is_private_and_does_not_modify_old_inode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        std::fs::write(&path, "{}").expect("original");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("permissions");
        let old = dir.path().join("old.json");
        std::fs::hard_link(&path, &old).expect("old inode");
        AuthFile::with_path(path.clone())
            .write("provider", Some(&Credential::ApiKey { key: "test".into() }))
            .expect("replace");
        assert_eq!(std::fs::read_to_string(old).expect("old content"), "{}");
        assert_eq!(
            std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

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
        // Imported provider shapes round-trip without losing the tag.
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
