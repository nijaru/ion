//! User settings (`~/.config/ion/settings.toml`), pi-style camelCase
//! keys. The compiled-in defaults mirror the maintainer's pi settings
//! (`gpt-5.6-luna` via openai-codex); a settings file overrides them.

use std::path::PathBuf;

use ion_core::SandboxMode;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    /// Follow the terminal's light/dark preference (pi's "light/dark");
    /// resolution currently maps to the dark palette.
    #[serde(rename = "light/dark")]
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    #[must_use]
    pub fn reasoning_effort(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Xhigh => Some("xhigh"),
            Self::Max => Some("max"),
        }
    }
}

/// Per-action key overrides; unset actions keep their defaults.
/// Key strings: modifiers `ctrl+`/`alt+`/`shift+` plus a key name
/// (letter, `enter`, `esc`, `tab`, `backspace`, `delete`, `up`,
/// `down`, `left`, `right`, `home`, `end`).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Keybindings {
    pub quit: Option<String>,
    pub cancel: Option<String>,
    pub submit: Option<String>,
    pub insert_newline: Option<String>,
    pub complete: Option<String>,
    pub history_previous: Option<String>,
    pub history_next: Option<String>,
    pub cursor_left: Option<String>,
    pub cursor_right: Option<String>,
    pub cursor_home: Option<String>,
    pub cursor_end: Option<String>,
    pub kill_to_end: Option<String>,
    pub kill_to_start: Option<String>,
    pub kill_word: Option<String>,
    pub yank: Option<String>,
    pub undo: Option<String>,
    pub toggle_tool_output: Option<String>,
    pub toggle_thinking: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    default_model: Option<String>,
    default_provider: Option<String>,
    /// Optional finite model list supplied by the host. Providers do not
    /// need to implement model enumeration for the TUI selector to work.
    #[serde(default)]
    model_catalog: Vec<String>,
    default_thinking_level: Option<ThinkingLevel>,
    /// Native shell enforcement. `auto` selects the strongest backend
    /// available on the host; it never upgrades project trust or approval.
    sandbox: Option<SandboxMode>,
    theme: Option<Theme>,
    #[serde(default)]
    pub keybindings: Keybindings,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    /// Names of configured MCP servers whose tools may enter model-step
    /// capability snapshots. An omitted/empty set keeps all MCP tools
    /// inactive until the host explicitly selects them.
    #[serde(default)]
    pub active_mcp_servers: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<ExtensionConfig>,
    /// Hide reasoning output in the TUI (pi-parity hideThinkingBlock).
    #[serde(default)]
    pub hide_thinking_block: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelection {
    pub provider: String,
    pub model: String,
}

/// One `[[extensions]]` entry: a subprocess extension publishing tools
/// (DESIGN.md §24). User-level configuration is trusted by being
/// user-authored.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtensionConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// One `[[mcp_servers]]` entry: a stdio MCP server launched at
/// startup (DESIGN.md §19).
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl From<McpServerConfig> for ion_core::ServerDef {
    fn from(config: McpServerConfig) -> Self {
        Self {
            name: config.name,
            command: config.command,
            args: config.args,
        }
    }
}

impl Settings {
    /// Compiled-in defaults, mirroring the maintainer's pi settings.
    /// Used only when no settings file exists; a file that omits a key
    /// means the key is unset.
    fn maintainer_defaults() -> Settings {
        Settings {
            default_model: Some("gpt-5.6-luna".to_owned()),
            default_provider: Some("openai-codex".to_owned()),
            model_catalog: Vec::new(),
            default_thinking_level: Some(ThinkingLevel::Xhigh),
            sandbox: None,
            theme: None,
            keybindings: Keybindings::default(),
            mcp_servers: Vec::new(),
            active_mcp_servers: Vec::new(),
            extensions: Vec::new(),
            hide_thinking_block: true,
        }
    }
    pub fn path() -> Option<PathBuf> {
        // Test/isolation override.
        if let Some(path) = std::env::var_os("ION_SETTINGS") {
            return Some(PathBuf::from(path));
        }
        let base = etcetera::base_strategy::choose_base_strategy().ok()?;
        use etcetera::base_strategy::BaseStrategy;
        Some(base.config_dir().join("ion").join("settings.toml"))
    }

    /// A settings value with everything unset - tests and hosts that
    /// compose their own configuration.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            default_model: None,
            default_provider: None,
            model_catalog: Vec::new(),
            default_thinking_level: None,
            sandbox: None,
            theme: None,
            keybindings: crate::settings::Keybindings::default(),
            mcp_servers: Vec::new(),
            active_mcp_servers: Vec::new(),
            extensions: Vec::new(),
            hide_thinking_block: false,
        }
    }

    /// Load settings; a missing file yields the defaults. A malformed
    /// file is an error, never silently ignored.
    pub fn load() -> Result<Settings, String> {
        let Some(path) = Self::path() else {
            return Ok(Self::maintainer_defaults());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::maintainer_defaults());
            }
            Err(err) => return Err(format!("{}: {err}", path.display())),
        };
        toml::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))
    }

    /// Resolve the configured provider and model. A provider prefix on the
    /// model is accepted when it agrees with `defaultProvider`.
    pub fn model_selection(&self) -> Result<Option<ModelSelection>, String> {
        let provider = self.default_provider.as_deref().unwrap_or("openai-codex");
        if !matches!(provider, "openai-codex" | "openrouter") {
            return Err(format!(
                "unsupported defaultProvider {:?}; supported providers are \"openai-codex\" and \"openrouter\"",
                self.default_provider
            ));
        }
        let Some(model) = &self.default_model else {
            return Ok(None);
        };
        let prefix = format!("{provider}/");
        if let Some(model) = model.strip_prefix(&prefix) {
            return Ok(Some(ModelSelection {
                provider: provider.to_owned(),
                model: model.to_owned(),
            }));
        }
        if model.starts_with("openai-codex/") || model.starts_with("openrouter/") {
            return Err(format!(
                "defaultModel provider prefix does not match defaultProvider: {model:?} vs {provider:?}"
            ));
        }
        Ok(Some(ModelSelection {
            provider: provider.to_owned(),
            model: model.to_owned(),
        }))
    }

    /// Return the host-supplied finite model list, including the launch
    /// default exactly once. Entries are displayable model references; the
    /// runtime/provider resolver remains authoritative for whether a switch
    /// can execute.
    pub fn model_catalog(&self) -> Result<Vec<String>, String> {
        let default = self.model_selection()?.map(|selection| selection.model);
        let mut catalog =
            Vec::with_capacity(self.model_catalog.len() + usize::from(default.is_some()));
        if let Some(model) = default {
            catalog.push(model);
        }
        for model in &self.model_catalog {
            let model = model.trim();
            if model.is_empty() {
                return Err("modelCatalog entries cannot be empty".to_owned());
            }
            if !catalog.iter().any(|candidate| candidate == model) {
                catalog.push(model.to_owned());
            }
        }
        Ok(catalog)
    }

    pub fn theme(&self) -> Theme {
        self.theme.unwrap_or(Theme::Auto)
    }

    #[must_use]
    pub fn thinking_level(&self) -> ThinkingLevel {
        self.default_thinking_level.unwrap_or(ThinkingLevel::Xhigh)
    }

    /// The resolved native-shell enforcement requested by this settings
    /// source. `Auto` is resolved when the tool catalog is constructed.
    #[must_use]
    pub fn sandbox_mode(&self) -> SandboxMode {
        self.sandbox.unwrap_or(SandboxMode::Auto)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_mirror_maintainer_pi_settings() {
        let settings = Settings::maintainer_defaults();
        assert_eq!(
            settings.model_selection().unwrap(),
            Some(ModelSelection {
                provider: "openai-codex".to_owned(),
                model: "gpt-5.6-luna".to_owned(),
            })
        );
        assert_eq!(settings.theme(), Theme::Auto);
        assert_eq!(settings.thinking_level(), ThinkingLevel::Xhigh);
    }

    #[test]
    fn parses_camel_case_keys_and_strips_provider_prefix() {
        let settings: Settings = toml::from_str(
            r#"
            defaultModel = "openrouter/stealth/ox-alpha"
            defaultProvider = "openrouter"
            modelCatalog = ["stealth/ox-alpha", "stealth/ox-beta"]
            theme = "light"
            "#,
        )
        .unwrap();
        assert_eq!(
            settings.model_selection().unwrap().unwrap().model,
            "stealth/ox-alpha"
        );
        assert_eq!(
            settings.model_catalog().unwrap(),
            ["stealth/ox-alpha", "stealth/ox-beta"]
        );
        assert_eq!(settings.theme(), Theme::Light);
    }

    #[test]
    fn no_default_model_falls_back_to_scripted() {
        let settings: Settings = toml::from_str("theme = \"dark\"").unwrap();
        assert_eq!(settings.model_selection().unwrap(), None);
        assert_eq!(settings.model_catalog().unwrap(), Vec::<String>::new());
        assert_eq!(settings.theme(), Theme::Dark);
    }

    #[test]
    fn empty_model_catalog_entry_is_refused() {
        let settings: Settings =
            toml::from_str("defaultModel = \"one\"\nmodelCatalog = [\"\", \"two\"]").unwrap();
        assert!(settings.model_catalog().is_err());
    }

    #[test]
    fn unsupported_provider_is_refused() {
        let settings: Settings = toml::from_str("defaultProvider = \"anthropic\"").unwrap();
        assert!(settings.model_selection().is_err());
    }

    #[test]
    fn codex_provider_is_supported() {
        let settings: Settings =
            toml::from_str("defaultModel = \"gpt-5.6-sol\"\ndefaultProvider = \"openai-codex\"")
                .unwrap();
        let selection = settings.model_selection().unwrap().unwrap();
        assert_eq!(selection.provider, "openai-codex");
        assert_eq!(selection.model, "gpt-5.6-sol");
    }

    #[test]
    fn parses_pi_thinking_level() {
        let settings: Settings = toml::from_str("defaultThinkingLevel = \"high\"").unwrap();
        assert_eq!(settings.thinking_level(), ThinkingLevel::High);
        assert_eq!(settings.thinking_level().reasoning_effort(), Some("high"));
    }

    #[test]
    fn parses_sandbox_mode() {
        let settings: Settings = toml::from_str("sandbox = \"seatbelt\"").unwrap();
        assert_eq!(settings.sandbox_mode(), SandboxMode::Seatbelt);
    }

    #[test]
    fn parses_explicit_active_mcp_servers() {
        let settings: Settings = toml::from_str(
            r#"
            activeMcpServers = ["docs", "repo"]
            "#,
        )
        .unwrap();
        assert_eq!(settings.active_mcp_servers, ["docs", "repo"]);
    }

    #[test]
    fn malformed_file_is_an_error() {
        let result: Result<Settings, _> = toml::from_str("defaultModel = 42");
        assert!(result.is_err());
    }
}

/// Extension definitions for one run (§24): user-level configuration
/// always loads; a project `.ion/extensions.toml` is executable
/// configuration from the workspace and loads only under an explicit
/// trust grant (§24.5). A skipped project manifest is announced, never
/// silent.
#[must_use]
pub fn load_extension_defs(
    settings: &Settings,
    project_root: Option<&std::path::Path>,
    trust_project: bool,
) -> Vec<ion_core::ExtensionDef> {
    let mut defs: Vec<ion_core::ExtensionDef> = settings
        .extensions
        .iter()
        .cloned()
        .map(|config| ion_core::ExtensionDef {
            name: config.name,
            command: config.command,
            args: config.args,
        })
        .collect();

    let Some(root) = project_root else {
        return defs;
    };
    let path = root.join(".ion").join("extensions.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return defs,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "project extensions unreadable");
            return defs;
        }
    };
    if !trust_project {
        tracing::warn!(
            path = %path.display(),
            "project extensions present but this workspace is not trusted; \
             pass --trust-project to enable them"
        );
        return defs;
    }
    match toml::from_str::<ProjectExtensions>(&text) {
        Ok(project) => {
            for config in project.extensions {
                defs.push(ion_core::ExtensionDef {
                    name: config.name,
                    command: config.command,
                    args: config.args,
                });
            }
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "project extensions malformed");
        }
    }
    defs
}

#[derive(Debug, Deserialize)]
struct ProjectExtensions {
    #[serde(default)]
    extensions: Vec<ExtensionConfig>,
}
