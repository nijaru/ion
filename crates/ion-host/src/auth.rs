//! Host-owned credentials for the CLI. Session history never contains keys.

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ion_ai::BoxFuture;
use ion_core::{CredentialResolutionError, CredentialResolver};
use tokio_util::sync::CancellationToken;

const MAX_KEY_BYTES: u64 = 4096;

#[derive(Clone)]
pub struct CredentialStore {
    root: PathBuf,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CredentialStatus {
    Environment,
    Saved,
    Missing,
}

impl CredentialStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Save a key for one provider. The provider name is a local identifier,
    /// never an endpoint or an arbitrary filename.
    pub fn save_api_key(&self, provider: &str, key: &str) -> Result<()> {
        validate_key(key)?;
        let path = self.key_path(provider)?;
        self.ensure_private_root()?;
        let random = random_bytes::<16>()?;
        let temp = path.with_extension(format!("{}.tmp", URL_SAFE_NO_PAD.encode(random)));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .context("cannot create private credential file")?;
        let result = (|| {
            file.write_all(key.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temp, &path)?;
            File::open(&self.root)?.sync_all()?;
            Ok::<_, std::io::Error>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.context("cannot save credential")
    }

    pub fn remove(&self, provider: &str) -> Result<()> {
        let path = self.key_path(provider)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("cannot remove credential"),
        }
    }

    pub fn status(&self, provider: &str, env_name: Option<&str>) -> Result<CredentialStatus> {
        if let Some(env_name) = env_name
            && environment_key(env_name)?.is_some()
        {
            return Ok(CredentialStatus::Environment);
        }
        Ok(if self.load_api_key(provider)?.is_some() {
            CredentialStatus::Saved
        } else {
            CredentialStatus::Missing
        })
    }

    /// Resolve at request time, so environment changes and logout take effect
    /// without copying credentials into a Session or long-lived request config.
    pub fn resolver(
        &self,
        provider: &str,
        env_name: Option<&str>,
    ) -> Result<Arc<dyn CredentialResolver>> {
        self.key_path(provider)?;
        ensure!(env_name != Some(""), "credential environment name is empty");
        Ok(Arc::new(StoredResolver {
            store: self.clone(),
            provider: provider.to_owned(),
            env_name: env_name.map(str::to_owned),
        }))
    }

    fn key_path(&self, provider: &str) -> Result<PathBuf> {
        ensure!(
            !provider.is_empty()
                && provider.len() <= 64
                && provider
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            "invalid provider identifier"
        );
        Ok(self.root.join(format!("{provider}.key")))
    }

    fn ensure_private_root(&self) -> Result<()> {
        if !self.root.exists() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.root)
                .context("cannot create credential directory")?;
        }
        let meta = fs::symlink_metadata(&self.root)?;
        ensure!(
            meta.file_type().is_dir() && meta.mode() & 0o077 == 0,
            "credential directory must be a private directory"
        );
        Ok(())
    }

    fn load_api_key(&self, provider: &str) -> Result<Option<String>> {
        let path = self.key_path(provider)?;
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("cannot inspect saved credential"),
        };
        ensure!(
            meta.file_type().is_file() && meta.mode() & 0o077 == 0,
            "saved credential must be a private regular file"
        );
        ensure!(meta.len() <= MAX_KEY_BYTES, "saved credential is too large");
        let key = fs::read_to_string(path).context("cannot read saved credential")?;
        validate_key(&key)?;
        Ok(Some(key))
    }
}

struct StoredResolver {
    store: CredentialStore,
    provider: String,
    env_name: Option<String>,
}

impl CredentialResolver for StoredResolver {
    fn resolve<'a>(
        &'a self,
        stop: CancellationToken,
    ) -> BoxFuture<'a, std::result::Result<Option<String>, CredentialResolutionError>> {
        Box::pin(async move {
            if stop.is_cancelled() {
                return Err(CredentialResolutionError::Cancelled);
            }
            let key = self
                .env_name
                .as_deref()
                .map_or(Ok(None), environment_key)
                .and_then(|key| match key {
                    Some(key) => Ok(Some(key)),
                    None => self.store.load_api_key(&self.provider),
                })
                .map_err(|_| CredentialResolutionError::Unavailable)?;
            if stop.is_cancelled() {
                return Err(CredentialResolutionError::Cancelled);
            }
            Ok(key)
        })
    }
}

fn environment_key(name: &str) -> Result<Option<String>> {
    ensure!(!name.is_empty(), "credential environment name is empty");
    match std::env::var(name) {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => {
            validate_key(&value)?;
            Ok(Some(value))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("credential environment value is not UTF-8")
        }
    }
}

fn validate_key(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty()
            && key.len() <= MAX_KEY_BYTES as usize
            && !key.chars().any(char::is_control),
        "credential is empty, too large, or contains control characters"
    );
    Ok(())
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("secure random source is unavailable"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn saves_private_key_and_rejects_symlink() {
        let root = std::env::temp_dir().join(format!("ion-auth-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = CredentialStore::new(&root);
        store.save_api_key("openrouter", "test-secret").unwrap();
        assert_eq!(
            store.load_api_key("openrouter").unwrap().as_deref(),
            Some("test-secret")
        );
        assert_eq!(
            fs::metadata(root.join("openrouter.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        store.remove("openrouter").unwrap();
        assert!(store.load_api_key("openrouter").unwrap().is_none());
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join("openrouter.key")).unwrap();
        assert!(store.load_api_key("openrouter").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn unnamed_custom_key_only_uses_provider_login() {
        let root = std::env::temp_dir().join(format!("ion-auth-{}", uuid::Uuid::now_v7()));
        let store = CredentialStore::new(&root);
        assert_eq!(
            store.status("desktop", None).unwrap(),
            CredentialStatus::Missing
        );
        assert!(
            store
                .resolver("desktop", None)
                .unwrap()
                .resolve(CancellationToken::new())
                .await
                .unwrap()
                .is_none()
        );
        store.save_api_key("desktop", "local-secret").unwrap();
        assert_eq!(
            store.status("desktop", None).unwrap(),
            CredentialStatus::Saved
        );
        assert_eq!(
            store
                .resolver("desktop", None)
                .unwrap()
                .resolve(CancellationToken::new())
                .await
                .unwrap()
                .as_deref(),
            Some("local-secret")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
