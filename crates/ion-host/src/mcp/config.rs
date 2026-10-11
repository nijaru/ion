//! MCP configuration validation and serialized atomic publication.
use crate::model_setup::write_json;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, File},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};
use url::Url;

#[derive(Clone, Serialize)]
#[serde(untagged)]
pub enum McpServer {
    Stdio(McpStdioServer),
    Http(McpHttpServer),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpStdioServer {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpHttpServer {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token_env: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServers {
    servers: BTreeMap<String, Value>,
}

pub struct McpListing {
    pub servers: BTreeMap<String, McpServer>,
    pub diagnostics: Vec<String>,
}

pub struct McpConfig {
    path: PathBuf,
}

impl McpConfig {
    pub fn new(config_root: &Path) -> Self {
        Self {
            path: config_root.join("mcp.json"),
        }
    }

    pub fn list(&self) -> Result<McpListing> {
        let mut servers = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for (name, value) in self.read_raw()? {
            match parse_server(&name, value) {
                Ok(server) => {
                    servers.insert(name, server);
                }
                Err(error) => diagnostics.push(format!("{error:#}")),
            }
        }
        Ok(McpListing {
            servers,
            diagnostics,
        })
    }

    pub(super) fn load_startup(&self) -> Result<(BTreeMap<String, McpServer>, Vec<String>)> {
        let listing = self.list()?;
        Ok((listing.servers, listing.diagnostics))
    }

    fn read_raw(&self) -> Result<BTreeMap<String, Value>> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice::<RawServers>(&bytes)
                .with_context(|| format!("invalid MCP config {}", self.path.display()))?
                .servers),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error).context("cannot read MCP config"),
        }
    }

    fn writer_lock(&self) -> Result<File> {
        let root = self.path.parent().context("MCP config has no parent")?;
        DirBuilder::new().recursive(true).mode(0o700).create(root)?;
        // The separate lock inode survives atomic replacement of mcp.json.
        let lock = File::from(rustix::fs::open(
            root.join("mcp.lock"),
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )?);
        ensure!(lock.metadata()?.is_file(), "MCP lock is not a regular file");
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
        Ok(lock)
    }

    pub fn add(&self, name: &str, server: McpServer) -> Result<()> {
        validate_name(name)?;
        validate_server(&server)?;
        let _lock = self.writer_lock()?;
        let mut servers = self.read_raw()?;
        ensure!(
            !servers.contains_key(name),
            "MCP server {name} already exists"
        );
        servers.insert(name.to_owned(), serde_json::to_value(server)?);
        write_json(&self.path, &RawServers { servers })
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let _lock = self.writer_lock()?;
        let mut servers = self.read_raw()?;
        ensure!(
            servers.remove(name).is_some(),
            "MCP server {name} was not found"
        );
        write_json(&self.path, &RawServers { servers })
    }
}

fn parse_server(name: &str, value: Value) -> Result<McpServer> {
    (|| -> Result<McpServer> {
        validate_name(name)?;
        let object = value.as_object().context("MCP server must be an object")?;
        let server = match (object.contains_key("command"), object.contains_key("url")) {
            (true, false) => McpServer::Stdio(serde_json::from_value(value)?),
            (false, true) => McpServer::Http(serde_json::from_value(value)?),
            (true, true) => bail!("MCP server cannot combine command and URL"),
            (false, false) => bail!("MCP server requires command or URL"),
        };
        validate_server(&server)?;
        Ok(server)
    })()
    .with_context(|| format!("invalid MCP server {name:?}"))
}

pub(super) fn validate_server(server: &McpServer) -> Result<()> {
    match server {
        McpServer::Stdio(server) => {
            ensure!(!server.command.trim().is_empty(), "MCP command is empty")
        }
        McpServer::Http(server) => {
            let url = Url::parse(&server.url).context("invalid MCP server URL")?;
            ensure!(
                matches!(url.scheme(), "http" | "https"),
                "MCP server URL must use HTTP or HTTPS"
            );
            ensure!(
                url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
                "MCP server URL cannot contain userinfo or fragment"
            );
            if let Some(name) = &server.bearer_token_env {
                ensure!(
                    valid_env_name(name),
                    "invalid MCP bearer token environment variable name"
                );
            }
        }
    }
    Ok(())
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub(super) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        bail!("MCP name {name:?} must use only ASCII letters, numbers, '_' or '-'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn remote_config_requires_http_url_and_named_ambient_token() {
        let root = std::env::temp_dir().join(format!("ion-mcp-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let config = McpConfig::new(&root);
        let remote = |url: &str, bearer_token_env: Option<&str>| {
            McpServer::Http(McpHttpServer {
                url: url.into(),
                bearer_token_env: bearer_token_env.map(str::to_owned),
            })
        };
        assert!(
            config
                .add("bad-scheme", remote("file:///tmp/mcp", None))
                .is_err()
        );
        assert!(
            config
                .add("bad-env", remote("https://example.com/mcp", Some("1KEY")))
                .is_err()
        );
        config
            .add("remote", remote("https://example.com/mcp", Some("MCP_KEY")))
            .unwrap();
        assert!(
            matches!(&config.list().unwrap().servers["remote"], McpServer::Http(server) if server.bearer_token_env.as_deref() == Some("MCP_KEY"))
        );
        assert!(
            fs::read_to_string(&config.path)
                .unwrap()
                .contains("MCP_KEY")
        );
        config.remove("remote").unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_server_entry_does_not_hide_valid_startup_servers_or_get_discarded() {
        let root = std::env::temp_dir().join(format!("ion-mcp-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let config = McpConfig::new(&root);
        fs::write(
            &config.path,
            br#"{"servers":{"good":{"command":"echo","args":["ready"]},"bad":{"command":7}}}"#,
        )
        .unwrap();

        let (servers, diagnostics) = config.load_startup().unwrap();
        assert_eq!(servers.len(), 1);
        assert!(matches!(&servers["good"], McpServer::Stdio(server) if server.command == "echo"));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("invalid MCP server \"bad\""));
        assert!(diagnostics[0].contains("expected a string"));
        let listing = config.list().unwrap();
        assert_eq!(listing.servers.len(), 1);
        assert!(listing.servers.contains_key("good"));
        assert_eq!(listing.diagnostics.len(), 1);

        config.remove("good").unwrap();
        config
            .add(
                "new",
                McpServer::Stdio(McpStdioServer {
                    command: "echo".into(),
                    args: Vec::new(),
                }),
            )
            .unwrap();
        let saved = fs::read_to_string(&config.path).unwrap();
        assert!(saved.contains("\"bad\""));
        assert!(saved.contains("\"new\""));
        assert!(!saved.contains("\"good\""));

        // Invalid entries are still addressable by name, so the user can
        // repair the file without hand-editing unrelated JSON.
        config.remove("bad").unwrap();
        let listing = config.list().unwrap();
        assert_eq!(listing.servers.len(), 1);
        assert!(listing.servers.contains_key("new"));
        assert!(listing.diagnostics.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_configuration_mutations_preserve_acknowledged_updates() {
        let root = std::env::temp_dir().join(format!("ion-mcp-config-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let config = McpConfig::new(&root);
        let server = || {
            McpServer::Stdio(McpStdioServer {
                command: "python3".into(),
                args: Vec::new(),
            })
        };
        for index in 0..8 {
            config.add(&format!("old-{index}"), server()).unwrap();
        }
        let start = Arc::new(Barrier::new(16));
        let workers = (0..16)
            .map(|index| {
                let root = root.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    let config = McpConfig::new(&root);
                    start.wait();
                    if index < 8 {
                        config.add(&format!("new-{index}"), server())
                    } else {
                        config.remove(&format!("old-{}", index - 8))
                    }
                })
            })
            .collect::<Vec<_>>();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        let saved = config.list().unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(results.iter().all(Result::is_ok), "{results:?}");
        assert!(saved.diagnostics.is_empty());
        assert_eq!(
            saved.servers.keys().cloned().collect::<Vec<_>>(),
            (0..8)
                .map(|index| format!("new-{index}"))
                .collect::<Vec<_>>(),
            "acknowledged add/remove updates were lost or resurrected"
        );
    }
}
