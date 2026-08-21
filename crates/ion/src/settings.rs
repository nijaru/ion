//! User settings (`~/.config/ion/settings.toml`), pi-style camelCase
//! keys. The compiled-in defaults mirror the maintainer's pi settings
//! (`stealth/ox-alpha` via openrouter); a settings file overrides them.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    /// Follow the terminal's light/dark preference (pi's "light/dark").
    #[serde(rename = "light/dark")]
    Auto,
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
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    default_model: Option<String>,
    default_provider: Option<String>,
    theme: Option<Theme>,
    #[serde(default)]
    pub keybindings: Keybindings,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
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
            default_model: Some("stealth/ox-alpha".to_owned()),
            default_provider: Some("openrouter".to_owned()),
            theme: None,
            keybindings: Keybindings::default(),
            mcp_servers: Vec::new(),
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

    /// The OpenRouter model id to use by default, if any. The provider
    /// must be openrouter (the only adapter); an `openrouter/` prefix
    /// on the model id is accepted and stripped.
    pub fn openrouter_model(&self) -> Result<Option<String>, String> {
        if self.default_provider.as_deref().unwrap_or("openrouter") != "openrouter" {
            return Err(format!(
                "unsupported defaultProvider {:?}; only \"openrouter\" exists",
                self.default_provider
            ));
        }
        let Some(model) = &self.default_model else {
            return Ok(None);
        };
        Ok(Some(
            model
                .strip_prefix("openrouter/")
                .unwrap_or(model)
                .to_owned(),
        ))
    }

    pub fn theme(&self) -> Theme {
        self.theme.unwrap_or(Theme::Auto)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_mirror_maintainer_pi_settings() {
        let settings = Settings::maintainer_defaults();
        assert_eq!(
            settings.openrouter_model().unwrap().as_deref(),
            Some("stealth/ox-alpha")
        );
        assert_eq!(settings.theme(), Theme::Auto);
    }

    #[test]
    fn parses_camel_case_keys_and_strips_provider_prefix() {
        let settings: Settings = toml::from_str(
            r#"
            defaultModel = "openrouter/stealth/ox-alpha"
            defaultProvider = "openrouter"
            theme = "light"
            "#,
        )
        .unwrap();
        assert_eq!(
            settings.openrouter_model().unwrap().as_deref(),
            Some("stealth/ox-alpha")
        );
        assert_eq!(settings.theme(), Theme::Light);
    }

    #[test]
    fn no_default_model_falls_back_to_scripted() {
        let settings: Settings = toml::from_str("theme = \"dark\"").unwrap();
        assert_eq!(settings.openrouter_model().unwrap(), None);
        assert_eq!(settings.theme(), Theme::Dark);
    }

    #[test]
    fn non_openrouter_provider_is_refused() {
        let settings: Settings = toml::from_str("defaultProvider = \"anthropic\"").unwrap();
        assert!(settings.openrouter_model().is_err());
    }

    #[test]
    fn malformed_file_is_an_error() {
        let result: Result<Settings, _> = toml::from_str("defaultModel = 42");
        assert!(result.is_err());
    }
}
