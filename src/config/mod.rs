use crate::mcp::McpServerConfig;
use crate::provider::ProviderPrefs;
use crate::tool::ToolMode;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Permission configuration (loaded from config file).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PermissionConfig {
    /// Default mode (read, write, agi). Default: write.
    pub default_mode: Option<String>,
    /// Auto-approve all tool calls (--yes behavior). Default: false.
    pub auto_approve: Option<bool>,
    /// Allow operations outside CWD (--no-sandbox behavior). Default: false.
    pub allow_outside_cwd: Option<bool>,
}

impl PermissionConfig {
    /// Get the tool mode from config, defaulting to Write if not specified.
    pub fn mode(&self) -> ToolMode {
        match self.default_mode.as_deref() {
            Some("read") => ToolMode::Read,
            Some("agi") => ToolMode::Agi,
            _ => ToolMode::Write,
        }
    }
}

/// Optional API keys (fallback when env vars not set).
/// Users with env vars don't need this section.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ApiKeys {
    pub openrouter: Option<String>,
    pub anthropic: Option<String>,
    pub openai: Option<String>,
    pub google: Option<String>,
    pub groq: Option<String>,
}

impl ApiKeys {
    /// Get API key for a provider (returns None if not configured).
    pub fn get(&self, provider: &str) -> Option<&str> {
        match provider {
            "openrouter" => self.openrouter.as_deref(),
            "anthropic" => self.anthropic.as_deref(),
            "openai" => self.openai.as_deref(),
            "google" => self.google.as_deref(),
            "groq" => self.groq.as_deref(),
            _ => None,
        }
    }

    /// Set API key for a provider.
    pub fn set(&mut self, provider: &str, key: String) {
        match provider {
            "openrouter" => self.openrouter = Some(key),
            "anthropic" => self.anthropic = Some(key),
            "openai" => self.openai = Some(key),
            "google" => self.google = Some(key),
            "groq" => self.groq = Some(key),
            _ => {}
        }
    }

    /// Check if any key is configured.
    pub fn has_any(&self) -> bool {
        self.openrouter.is_some()
            || self.anthropic.is_some()
            || self.openai.is_some()
            || self.google.is_some()
            || self.groq.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Active provider (e.g., "openrouter", "anthropic", "google").
    pub provider: Option<String>,

    /// Selected model name (as the provider calls it, no prefix).
    #[serde(alias = "default_model")]
    pub model: Option<String>,

    /// Optional API keys (fallback when env vars not set).
    pub api_keys: ApiKeys,

    pub data_dir: PathBuf,

    /// Provider preferences for model filtering and routing.
    pub provider_prefs: ProviderPrefs,

    /// TTL for cached model list in seconds. Default: 3600 (1 hour).
    pub model_cache_ttl_secs: u64,

    /// MCP server configurations.
    pub mcp_servers: HashMap<String, McpServerConfig>,

    /// Permission settings.
    pub permissions: PermissionConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: None,
            model: None,
            api_keys: ApiKeys::default(),
            data_dir: ion_data_dir(),
            provider_prefs: ProviderPrefs::default(),
            model_cache_ttl_secs: 3600,
            mcp_servers: HashMap::new(),
            permissions: PermissionConfig::default(),
        }
    }
}

impl Config {
    /// Path to the sessions SQLite database.
    pub fn sessions_db_path(&self) -> PathBuf {
        self.data_dir.join("sessions.db")
    }

    /// Check if first-time setup is needed (no provider or model selected).
    pub fn needs_setup(&self) -> bool {
        self.provider.is_none() || self.model.is_none()
    }

    /// Get API key for a provider.
    /// Priority: config file > env var (explicit config is more intentional).
    pub fn api_key_for(&self, provider: &str) -> Option<String> {
        // Ollama doesn't need a key
        if provider == "ollama" {
            return Some(String::new());
        }

        // 1. Check config file first (explicit user configuration)
        if let Some(key) = self.api_keys.get(provider) {
            return Some(key.to_string());
        }

        // 2. Fall back to env var
        let env_vars: &[&str] = match provider {
            "openrouter" => &["OPENROUTER_API_KEY"],
            "anthropic" => &["ANTHROPIC_API_KEY"],
            "openai" => &["OPENAI_API_KEY"],
            "google" => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
            "groq" => &["GROQ_API_KEY"],
            _ => return None,
        };

        for var in env_vars {
            if let Ok(key) = std::env::var(var) {
                if !key.is_empty() {
                    return Some(key);
                }
            }
        }

        None
    }

    /// Load configuration with layered precedence.
    ///
    /// Precedence (highest to lowest):
    /// 1. Project local (.ion/config.local.toml) - gitignored
    /// 2. Project shared (.ion/config.toml) - committed
    /// 3. User global (~/.ion/config.toml)
    /// 4. Built-in defaults
    pub fn load() -> anyhow::Result<Self> {
        let mut config = Config::default();

        // Layer 1: User global (~/.ion/config.toml)
        let user_config = ion_config_dir().join("config.toml");
        if user_config.exists() {
            config.merge_from_file(&user_config)?;
        } else {
            // Migration: check old location
            migrate_old_config()?;
            if user_config.exists() {
                config.merge_from_file(&user_config)?;
            }
        }

        // Layer 2: Project shared (.ion/config.toml)
        let project_config = PathBuf::from(".ion/config.toml");
        if project_config.exists() {
            config.merge_from_file(&project_config)?;
        }

        // Layer 3: Project local (.ion/config.local.toml)
        let local_config = PathBuf::from(".ion/config.local.toml");
        if local_config.exists() {
            config.merge_from_file(&local_config)?;
        }

        Ok(config)
    }

    /// Merge config from a TOML file, overriding only non-None values.
    fn merge_from_file(&mut self, path: &Path) -> anyhow::Result<()> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading config from {}", path.display()))?;
        let other: Config = toml::from_str(&content)
            .with_context(|| format!("parsing config from {}", path.display()))?;
        self.merge(other);
        Ok(())
    }

    /// Merge another config into this one. Non-default values override.
    fn merge(&mut self, other: Config) {
        if other.provider.is_some() {
            self.provider = other.provider;
        }
        if other.model.is_some() {
            self.model = other.model;
        }
        // Merge API keys
        if other.api_keys.openrouter.is_some() {
            self.api_keys.openrouter = other.api_keys.openrouter;
        }
        if other.api_keys.anthropic.is_some() {
            self.api_keys.anthropic = other.api_keys.anthropic;
        }
        if other.api_keys.openai.is_some() {
            self.api_keys.openai = other.api_keys.openai;
        }
        if other.api_keys.google.is_some() {
            self.api_keys.google = other.api_keys.google;
        }
        if other.api_keys.groq.is_some() {
            self.api_keys.groq = other.api_keys.groq;
        }
        if other.data_dir != ion_data_dir() {
            self.data_dir = other.data_dir;
        }
        if other.model_cache_ttl_secs != 3600 {
            self.model_cache_ttl_secs = other.model_cache_ttl_secs;
        }
        if !other.mcp_servers.is_empty() {
            self.mcp_servers.extend(other.mcp_servers);
        }
        // Merge provider_prefs if any fields are set
        if other.provider_prefs.quantizations.is_some() {
            self.provider_prefs.quantizations = other.provider_prefs.quantizations;
        }
        if other.provider_prefs.exclude_quants.is_some() {
            self.provider_prefs.exclude_quants = other.provider_prefs.exclude_quants;
        }
        if other.provider_prefs.min_bits.is_some() {
            self.provider_prefs.min_bits = other.provider_prefs.min_bits;
        }
        if other.provider_prefs.ignore.is_some() {
            self.provider_prefs.ignore = other.provider_prefs.ignore;
        }
        if other.provider_prefs.only.is_some() {
            self.provider_prefs.only = other.provider_prefs.only;
        }
        if other.provider_prefs.prefer.is_some() {
            self.provider_prefs.prefer = other.provider_prefs.prefer;
        }
        if other.provider_prefs.order.is_some() {
            self.provider_prefs.order = other.provider_prefs.order;
        }
        // Merge permissions if any fields are set
        if other.permissions.default_mode.is_some() {
            self.permissions.default_mode = other.permissions.default_mode;
        }
        if other.permissions.auto_approve.is_some() {
            self.permissions.auto_approve = other.permissions.auto_approve;
        }
        if other.permissions.allow_outside_cwd.is_some() {
            self.permissions.allow_outside_cwd = other.permissions.allow_outside_cwd;
        }
    }

    /// Save configuration to user global config file (~/.ion/config.toml).
    pub fn save(&self) -> anyhow::Result<()> {
        let config_path = ion_config_dir().join("config.toml");

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)?;
        std::fs::write(&config_path, content)?;

        Ok(())
    }
}

/// ion config directory: ~/.ion/
pub fn ion_config_dir() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".ion"))
        .unwrap_or_else(|| PathBuf::from(".ion"))
}

/// ion data directory: ~/.ion/data/
pub fn ion_data_dir() -> PathBuf {
    ion_config_dir().join("data")
}

/// Universal agents directory: ~/.agents/ (proposed standard)
pub fn agents_dir() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".agents"))
        .unwrap_or_else(|| PathBuf::from(".agents"))
}

/// Migrate config from old location (~/.config/ion/) to new (~/.ion/).
fn migrate_old_config() -> anyhow::Result<()> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };

    let old_config = home.join(".config/ion/config.toml");
    let new_config = ion_config_dir().join("config.toml");

    if old_config.exists() && !new_config.exists() {
        // Create new config dir
        if let Some(parent) = new_config.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Copy config file
        std::fs::copy(&old_config, &new_config).with_context(|| {
            format!(
                "migrating config from {} to {}",
                old_config.display(),
                new_config.display()
            )
        })?;

        tracing::info!(
            "Migrated config from {} to {}",
            old_config.display(),
            new_config.display()
        );
    }

    // Migrate data directory
    let old_data = home.join(".local/share/ion");
    let new_data = ion_data_dir();

    if old_data.exists() && !new_data.exists() {
        std::fs::create_dir_all(&new_data)?;

        // Copy sessions.db if it exists
        let old_db = old_data.join("sessions.db");
        let new_db = new_data.join("sessions.db");
        if old_db.exists() {
            std::fs::copy(&old_db, &new_db).with_context(|| {
                format!(
                    "migrating sessions.db from {} to {}",
                    old_db.display(),
                    new_db.display()
                )
            })?;
            tracing::info!(
                "Migrated sessions.db from {} to {}",
                old_db.display(),
                new_db.display()
            );
        }
    }

    Ok(())
}

/// Load instruction files (AGENTS.md, CLAUDE.md).
///
/// Loading order:
/// 1. ./AGENTS.md (project root, primary)
/// 2. ./CLAUDE.md (project root, fallback - only if AGENTS.md not found)
/// 3. ~/.agents/AGENTS.md (user global, preferred)
/// 4. ~/.ion/AGENTS.md (user global, fallback - only if ~/.agents/AGENTS.md not found)
///
/// Returns project instructions + user instructions (max 2 files).
pub fn load_instructions(working_dir: &Path) -> String {
    let mut instructions = String::new();

    // Project level: first found wins
    let project_agents = working_dir.join("AGENTS.md");
    let project_claude = working_dir.join("CLAUDE.md");

    if project_agents.exists() {
        if let Ok(content) = std::fs::read_to_string(&project_agents) {
            instructions.push_str(&content);
            instructions.push_str("\n\n");
        }
    } else if project_claude.exists() {
        if let Ok(content) = std::fs::read_to_string(&project_claude) {
            instructions.push_str(&content);
            instructions.push_str("\n\n");
        }
    }

    // User level: ~/.agents/AGENTS.md preferred, ~/.ion/AGENTS.md fallback
    let user_agents_global = agents_dir().join("AGENTS.md");
    let user_ion_agents = ion_config_dir().join("AGENTS.md");

    if user_agents_global.exists() {
        if let Ok(content) = std::fs::read_to_string(&user_agents_global) {
            if !instructions.is_empty() {
                instructions.push_str("---\n\n");
            }
            instructions.push_str(&content);
        }
    } else if user_ion_agents.exists() {
        if let Ok(content) = std::fs::read_to_string(&user_ion_agents) {
            if !instructions.is_empty() {
                instructions.push_str("---\n\n");
            }
            instructions.push_str(&content);
        }
    }

    instructions
}

/// Ensure local config files are gitignored.
/// Call this when creating .ion/config.local.toml.
pub fn ensure_local_gitignored() -> anyhow::Result<()> {
    let gitignore = PathBuf::from(".gitignore");
    let pattern = ".ion/*.local.toml";

    if gitignore.exists() {
        let content = std::fs::read_to_string(&gitignore)?;
        if !content.contains(pattern) {
            let mut new_content = content;
            if !new_content.ends_with('\n') {
                new_content.push('\n');
            }
            new_content.push_str("\n# ion local config\n");
            new_content.push_str(pattern);
            new_content.push('\n');
            std::fs::write(&gitignore, new_content)?;
            tracing::info!("Added {} to .gitignore", pattern);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.provider.is_none());
        assert!(config.model.is_none());
        assert!(config.needs_setup());
    }

    #[test]
    fn test_merge_configs() {
        let mut base = Config::default();
        let mut api_keys = ApiKeys::default();
        api_keys.openrouter = Some("test-key".to_string());
        let other = Config {
            provider: Some("openrouter".to_string()),
            model: Some("test-model".to_string()),
            api_keys,
            ..Default::default()
        };

        base.merge(other);
        assert_eq!(base.provider, Some("openrouter".to_string()));
        assert_eq!(base.model, Some("test-model".to_string()));
        assert_eq!(base.api_keys.openrouter, Some("test-key".to_string()));
    }

    #[test]
    fn test_load_instructions() {
        let temp_dir = TempDir::new().unwrap();
        let agents_path = temp_dir.path().join("AGENTS.md");
        std::fs::write(&agents_path, "# Test Instructions").unwrap();

        let instructions = load_instructions(temp_dir.path());
        assert!(instructions.contains("# Test Instructions"));
    }

    #[test]
    fn test_claude_md_fallback() {
        let temp_dir = TempDir::new().unwrap();
        let claude_path = temp_dir.path().join("CLAUDE.md");
        std::fs::write(&claude_path, "# Claude Instructions").unwrap();

        let instructions = load_instructions(temp_dir.path());
        assert!(instructions.contains("# Claude Instructions"));
    }

    #[test]
    fn test_agents_md_takes_priority() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("AGENTS.md"), "# Agents").unwrap();
        std::fs::write(temp_dir.path().join("CLAUDE.md"), "# Claude").unwrap();

        let instructions = load_instructions(temp_dir.path());
        assert!(instructions.contains("# Agents"));
        assert!(!instructions.contains("# Claude"));
    }
}
