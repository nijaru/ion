//! Host-owned credentials for the CLI. Session history never contains keys.
//!
//! OpenRouter's documented PKCE flow returns an ordinary API key usable on its
//! Chat Completions endpoint: https://openrouter.ai/docs/guides/overview/auth/oauth

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ion_ai::BoxFuture;
use ion_core::{CredentialResolutionError, CredentialResolver};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

const MAX_KEY_BYTES: u64 = 4096;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
const EXCHANGE_URL: &str = "https://openrouter.ai/api/v1/auth/keys";

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

    pub fn status(&self, provider: &str, env_name: &str) -> Result<CredentialStatus> {
        if environment_key(env_name)?.is_some() {
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
    pub fn resolver(&self, provider: &str, env_name: &str) -> Result<Arc<dyn CredentialResolver>> {
        self.key_path(provider)?;
        ensure!(!env_name.is_empty(), "credential environment name is empty");
        Ok(Arc::new(StoredResolver {
            store: self.clone(),
            provider: provider.to_owned(),
            env_name: env_name.to_owned(),
        }))
    }

    /// Opens the documented OpenRouter localhost PKCE route. Always prints the
    /// URL so login remains possible when no browser opener is installed.
    pub async fn login_openrouter(&self) -> Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .context("cannot start local OpenRouter login callback")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let nonce = URL_SAFE_NO_PAD.encode(random_bytes::<24>()?);
        let verifier = URL_SAFE_NO_PAD.encode(random_bytes::<32>()?);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        // OpenRouter does not document a `state` parameter. An unpredictable
        // callback path binds this listener to the login it initiated.
        let callback_path = format!("/callback/{nonce}");
        let callback = format!("http://localhost:{port}{callback_path}");
        let mut url = reqwest::Url::parse("https://openrouter.ai/auth")?;
        url.query_pairs_mut()
            .append_pair("callback_url", &callback)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("key_label", "Ion");
        eprintln!("Open this URL to sign in to OpenRouter:\n{url}");
        open_browser(url.as_str());
        let code = wait_for_code(listener, &callback_path).await?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        let response = client
            .post(EXCHANGE_URL)
            .json(&json!({
                "code": code,
                "code_verifier": verifier,
                "code_challenge_method": "S256",
            }))
            .send()
            .await
            .context("OpenRouter code exchange failed")?;
        ensure!(
            response.status().is_success(),
            "OpenRouter rejected the authorization code"
        );
        ensure!(
            response.content_length().is_none_or(|len| len <= 8192),
            "OpenRouter login response is too large"
        );
        let body = response
            .bytes()
            .await
            .context("cannot read OpenRouter login response")?;
        ensure!(body.len() <= 8192, "OpenRouter login response is too large");
        #[derive(Deserialize)]
        struct KeyResponse {
            key: String,
        }
        let key: KeyResponse = serde_json::from_slice(&body)
            .context("OpenRouter login response did not contain a key")?;
        self.save_api_key("openrouter", &key.key)
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
    env_name: String,
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
            let key = environment_key(&self.env_name)
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

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(not(target_os = "macos"))]
    let command = "xdg-open";
    let _ = Command::new(command)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

async fn wait_for_code(listener: TcpListener, path: &str) -> Result<String> {
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        ensure!(Instant::now() < deadline, "OpenRouter login timed out");
        match listener.accept() {
            Ok((stream, _)) => {
                let expected = path.to_owned();
                let result =
                    tokio::task::spawn_blocking(move || receive_callback(stream, &expected))
                        .await
                        .context("OpenRouter callback task failed")??;
                if let Some(code) = result {
                    return Ok(code);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error).context("OpenRouter callback listener failed"),
        }
    }
}

fn receive_callback(mut stream: TcpStream, expected_path: &str) -> Result<Option<String>> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    while request.len() < 8192 && !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => request.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    let request = String::from_utf8(request).ok();
    let code = request.as_deref().and_then(|request| {
        let line = request.lines().next()?;
        let target = line.strip_prefix("GET ")?.split_once(' ')?.0;
        let url = reqwest::Url::parse(&format!("http://localhost{target}")).ok()?;
        if url.path() != expected_path {
            return None;
        }
        let mut codes = url.query_pairs().filter(|(key, _)| key == "code");
        let code = codes.next()?.1.into_owned();
        (codes.next().is_none() && !code.is_empty() && code.len() <= 4096).then_some(code)
    });
    let (status, body) = if code.is_some() {
        (
            "200 OK",
            "Ion received the authorization code. Return to the terminal.",
        )
    } else {
        ("400 Bad Request", "Invalid Ion login callback.")
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    Ok(code)
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

    #[test]
    fn callback_accepts_only_its_path_and_one_code() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let writer = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .write_all(b"GET /callback/nonce?code=abc HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .unwrap();
        });
        let (stream, _) = listener.accept().unwrap();
        assert_eq!(
            receive_callback(stream, "/callback/nonce")
                .unwrap()
                .as_deref(),
            Some("abc")
        );
        writer.join().unwrap();
    }
}
