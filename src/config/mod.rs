use crate::mcp::McpServerConfig;
use crate::provider::ProviderPrefs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub openrouter_api_key: Option<String>,
    pub anthropic_api_key: Option<String>,
    /// User's selected model. None until first setup.
    pub default_model: Option<String>,
    pub data_dir: PathBuf,

    /// Provider preferences for model filtering and routing.
    pub provider_prefs: ProviderPrefs,

    /// TTL for cached model list in seconds. Default: 3600 (1 hour).
    pub model_cache_ttl_secs: u64,

    /// MCP server configurations.
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        let data_dir = directories::ProjectDirs::from("com", "nijaru", "ion")
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".ion"));

        Self {
            openrouter_api_key: None,
            anthropic_api_key: None,
            default_model: None,
            data_dir,
            provider_prefs: ProviderPrefs::default(),
            model_cache_ttl_secs: 3600,
            mcp_servers: HashMap::new(),
        }
    }
}

impl Config {
    /// Path to the sessions SQLite database.
    pub fn sessions_db_path(&self) -> PathBuf {
        self.data_dir.join("sessions.db")
    }

    /// Check if first-time setup is needed (no API key or no model selected).
    pub fn needs_setup(&self) -> bool {
        let has_api_key = self.openrouter_api_key.is_some() || self.anthropic_api_key.is_some();
        !has_api_key || self.default_model.is_none()
    }

    /// Check if any API provider is configured.
    pub fn has_api_key(&self) -> bool {
        self.openrouter_api_key.is_some() || self.anthropic_api_key.is_some()
    }

    pub fn load() -> anyhow::Result<Self> {
        let config_path = directories::ProjectDirs::from("com", "nijaru", "ion")
            .map(|d| d.config_dir().join("config.toml"))
            .unwrap_or_else(|| PathBuf::from(".ion/config.toml"));

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }
}
