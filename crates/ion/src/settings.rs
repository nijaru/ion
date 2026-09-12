//! User settings (`~/.config/ion/settings.toml`), pi-style camelCase
//! keys. The compiled-in defaults mirror the maintainer's local workflow
//! (`qwen3.8:27b` via desktop); a settings file overrides them.

use std::path::PathBuf;

use ion_core::SandboxMode;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    /// Follow the terminal's light/dark preference (pi's "light/dark");
    /// resolution currently maps to the dark palette.
    #[default]
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
    pub kill_word_forward: Option<String>,
    pub yank: Option<String>,
    pub yank_pop: Option<String>,
    pub cursor_word_left: Option<String>,
    pub cursor_word_right: Option<String>,
    pub external_editor: Option<String>,
    pub undo: Option<String>,
    pub toggle_tool_output: Option<String>,
    pub toggle_thinking: Option<String>,
    /// Read the system clipboard for the composer (ctrl+v default).
    pub paste_clipboard: Option<String>,
    /// Copy the last assistant message to the clipboard (ctrl+x
    /// default).
    pub copy_last_message: Option<String>,
}

/// Interactive TUI mode (pi parity: tuiMode setting + --tui-mode flag).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
pub enum TuiMode {
    #[default]
    Regular,
    Fullscreen,
}

/// Pi hides model reasoning blocks by default; an omitted key must do
/// the same so an empty or partial settings file matches pi behavior.
fn hide_thinking_block_default() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    default_model: Option<String>,
    default_provider: Option<String>,
    /// Base URL for the local OpenAI-compatible desktop provider.
    desktop_base_url: Option<String>,
    /// Optional bearer key for the local desktop provider. Environment
    /// configuration takes precedence so credentials need not be stored in
    /// this file.
    desktop_api_key: Option<String>,
    /// Optional finite model list supplied by the host. Providers do not
    /// need to implement model enumeration for the TUI selector to work.
    #[serde(default)]
    model_catalog: Vec<String>,
    default_thinking_level: Option<ThinkingLevel>,
    /// Native shell enforcement. `auto` selects the strongest backend
    /// available on the host; it never upgrades project trust or approval.
    sandbox: Option<SandboxMode>,
    /// Workspace path policy for native file tools. `off` (default,
    /// pi parity) resolves any absolute path — protection belongs to
    /// policy and protected-path rules; `workspace` confines
    /// mutations to the project root.
    workspace_sandbox: Option<WorkspaceSandboxMode>,
    /// File mutations (write/edit) deny on these path entries wherever
    /// they appear, exactly pi's protected-paths extension. The default
    /// is pi's list: .env files, .git, node_modules, chezmoi data,
    /// age keys. Reads never deny.
    protected_paths: Option<Vec<String>>,
    theme: Option<Theme>,
    /// Interactive TUI mode: `"regular"` (default) or `"fullscreen"`
    /// (pi parity: alt-screen transcript with search). The `--tui-mode`
    /// flag overrides this at startup; `/fullscreen` toggles live.
    tui_mode: Option<TuiMode>,
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
    /// Expose the family agent-control tools to the model-facing catalog.
    /// Disabled by default, matching pi's opt-in subagent extension.
    #[serde(default)]
    enable_agents: bool,
    /// Hide reasoning output in the TUI (pi-parity hideThinkingBlock,
    /// which pi defaults to true).
    #[serde(default = "hide_thinking_block_default")]
    pub hide_thinking_block: bool,
    /// Transient provider-failure retry (pi-parity `settings.retry`).
    #[serde(default)]
    retry: RetrySettings,
    /// Print a notice when a settled turn re-bills prompt tokens that
    /// the previous turn had cached (pi-parity showCacheMissNotices,
    /// which pi defaults to false).
    #[serde(default)]
    show_cache_miss_notices: bool,
}

/// `[retry]`: pi's `settings.retry` grammar. Defaults mirror pi's
/// enabled / 3 retries / 2s exponential base.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct RetrySettings {
    pub enabled: Option<bool>,
    pub max_retries: Option<u32>,
    pub base_delay_ms: Option<u64>,
}

impl RetrySettings {
    /// Resolve into the core policy, filling unset keys with pi's
    /// defaults (enabled, 3 retries, 2000ms base).
    #[must_use]
    pub fn resolve(&self) -> ion_core::RetryPolicy {
        ion_core::RetryPolicy {
            enabled: self.enabled.unwrap_or(true),
            max_retries: self.max_retries.unwrap_or(3),
            base_delay_ms: self.base_delay_ms.unwrap_or(2000),
        }
    }
}

/// Parse before editing so nested keys, arrays, comments, and string values
/// retain their TOML meaning. A missing file is an empty configuration;
/// malformed existing content is never silently repaired or overwritten.
fn edit_settings(
    path: &std::path::Path,
    edit: impl FnOnce(&mut toml_edit::DocumentMut),
) -> Result<std::path::PathBuf, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("{}: {err}", path.display())),
    };
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|err| format!("{}: {err}", path.display()))?;
    edit(&mut document);
    atomic_write(path, &document.to_string())
        .map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(path.to_owned())
}

fn set_value(document: &mut toml_edit::DocumentMut, key: &str, mut value: toml_edit::Value) {
    if let Some(previous) = document.get(key).and_then(toml_edit::Item::as_value) {
        *value.decor_mut() = previous.decor().clone();
    }
    document[key] = toml_edit::Item::Value(value);
}

fn atomic_write(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut pending = tempfile::NamedTempFile::new_in(dir)?;
    pending.write_all(text.as_bytes())?;
    pending.as_file().sync_all()?;
    pending.persist(path).map_err(|err| err.error)?;
    std::fs::File::open(dir)?.sync_all()
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

/// `workspaceSandbox`: where native file tools may resolve paths.
/// `off` is pi parity (default): any absolute path resolves.
/// `workspace` is ion's fail-closed posture: mutations confined to
/// the project root with `.git` protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceSandboxMode {
    #[default]
    Off,
    Workspace,
}

impl From<WorkspaceSandboxMode> for ion_core::WorkspacePolicy {
    fn from(mode: WorkspaceSandboxMode) -> Self {
        match mode {
            WorkspaceSandboxMode::Off => Self::Unrestricted,
            WorkspaceSandboxMode::Workspace => Self::Confined,
        }
    }
}

impl Settings {
    /// The resolved transient-failure retry policy for runtime
    /// composition.
    #[must_use]
    pub fn retry_policy(&self) -> ion_core::RetryPolicy {
        self.retry.resolve()
    }

    /// Whether cache-miss notices print at settle (pi's
    /// showCacheMissNotices, default false).
    #[must_use]
    pub fn show_cache_miss_notices(&self) -> bool {
        self.show_cache_miss_notices
    }

    /// Compiled-in defaults, mirroring the maintainer's pi settings.
    /// Used only when no settings file exists; a file that omits a key
    /// means the key is unset.
    fn maintainer_defaults() -> Settings {
        Settings {
            default_model: Some("qwen3.8:27b".to_owned()),
            default_provider: Some("desktop".to_owned()),
            desktop_base_url: Some("http://desktop:8080/v1".to_owned()),
            desktop_api_key: None,
            model_catalog: Vec::new(),
            default_thinking_level: Some(ThinkingLevel::Xhigh),
            sandbox: None,
            workspace_sandbox: None,
            protected_paths: None,
            theme: None,
            tui_mode: None,
            keybindings: Keybindings::default(),
            mcp_servers: Vec::new(),
            active_mcp_servers: Vec::new(),
            extensions: Vec::new(),
            enable_agents: false,
            hide_thinking_block: true,
            retry: RetrySettings::default(),
            show_cache_miss_notices: false,
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
            desktop_base_url: None,
            desktop_api_key: None,
            model_catalog: Vec::new(),
            default_thinking_level: None,
            sandbox: None,
            workspace_sandbox: None,
            protected_paths: None,
            theme: None,
            tui_mode: None,
            keybindings: crate::settings::Keybindings::default(),
            mcp_servers: Vec::new(),
            active_mcp_servers: Vec::new(),
            extensions: Vec::new(),
            enable_agents: false,
            hide_thinking_block: false,
            retry: RetrySettings::default(),
            show_cache_miss_notices: false,
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

    /// Whether model-facing family agent controls are enabled for this run.
    #[must_use]
    pub fn agents_enabled(&self) -> bool {
        self.enable_agents
    }

    /// Resolve the configured provider and model. A provider prefix on the
    /// model is accepted when it agrees with `defaultProvider`.
    pub fn model_selection(&self) -> Result<Option<ModelSelection>, String> {
        let provider = self.default_provider.as_deref().unwrap_or("openai-codex");
        if !matches!(provider, "openai-codex" | "openrouter" | "desktop") {
            return Err(format!(
                "unsupported defaultProvider {:?}; supported providers are \"openai-codex\", \"openrouter\", and \"desktop\"",
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
        if model.starts_with("openai-codex/")
            || model.starts_with("openrouter/")
            || model.starts_with("desktop/")
        {
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
        let default = self.model_selection()?;
        let provider = default.as_ref().map_or_else(
            || {
                self.default_provider
                    .as_deref()
                    .unwrap_or("openai-codex")
                    .to_owned()
            },
            |s| s.provider.clone(),
        );
        let mut catalog =
            Vec::with_capacity(self.model_catalog.len() + usize::from(default.is_some()));
        if let Some(selection) = default {
            catalog.push(format!("{}/{}", selection.provider, selection.model));
        }
        for model in &self.model_catalog {
            let model = model.trim();
            if model.is_empty() {
                return Err("modelCatalog entries cannot be empty".to_owned());
            }
            let qualified = if ["openai-codex/", "openrouter/", "desktop/"]
                .iter()
                .any(|prefix| model.starts_with(prefix))
            {
                model.to_owned()
            } else {
                format!("{provider}/{model}")
            };
            if !catalog.iter().any(|candidate| candidate == &qualified) {
                catalog.push(qualified);
            }
        }
        Ok(catalog)
    }

    /// Resolve the local OpenAI-compatible endpoint. Environment
    /// configuration is useful for machines where the desktop hostname or
    /// port differs from the maintainer default.
    pub fn desktop_base_url(&self) -> String {
        std::env::var("ION_DESKTOP_BASE_URL")
            .ok()
            .or_else(|| self.desktop_base_url.clone())
            .unwrap_or_else(|| "http://desktop:8080/v1".to_owned())
    }

    /// Resolve the optional local bearer key without requiring one for local
    /// servers that do not authenticate requests.
    pub fn desktop_api_key(&self) -> String {
        std::env::var("ION_DESKTOP_API_KEY")
            .ok()
            .or_else(|| self.desktop_api_key.clone())
            .unwrap_or_default()
    }

    pub fn theme(&self) -> Theme {
        self.theme.unwrap_or(Theme::Auto)
    }

    /// Save a picker value with its TOML type, preserving unrelated settings.
    pub fn write_setting(
        path: &std::path::Path,
        key: &str,
        value: &str,
    ) -> Result<std::path::PathBuf, String> {
        let value = match key {
            "hideThinkingBlock" | "showCacheMissNotices" => {
                toml_edit::Value::from(value.parse::<bool>().map_err(|err| err.to_string())?)
            }
            "theme" | "tuiMode" | "defaultThinkingLevel" => toml_edit::Value::from(value),
            _ => return Err(format!("unsupported picker setting: {key}")),
        };
        edit_settings(path, |document| set_value(document, key, value))
    }

    /// Save the provider/model pair without reserializing unrelated settings.
    pub fn write_default_model(
        path: &std::path::Path,
        provider: &str,
        model: &str,
    ) -> Result<std::path::PathBuf, String> {
        let bare_model = model.strip_prefix(&format!("{provider}/")).unwrap_or(model);
        edit_settings(path, |document| {
            set_value(document, "defaultProvider", provider.into());
            set_value(document, "defaultModel", bare_model.into());
        })
    }

    /// Save the scoped catalog; an empty selection removes the override.
    pub fn write_model_catalog(
        path: &std::path::Path,
        catalog: &[String],
    ) -> Result<std::path::PathBuf, String> {
        edit_settings(path, |document| {
            if catalog.is_empty() {
                document.as_table_mut().remove("modelCatalog");
            } else {
                let array = catalog
                    .iter()
                    .map(String::as_str)
                    .collect::<toml_edit::Array>();
                set_value(document, "modelCatalog", array.into());
            }
        })
    }

    /// Launch TUI mode: the `--tui-mode` flag overrides the setting.
    pub fn tui_mode(&self) -> TuiMode {
        self.tui_mode.unwrap_or_default()
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

    /// The workspace path policy for native file tools.
    #[must_use]
    pub fn workspace_policy(&self) -> ion_core::WorkspacePolicy {
        self.workspace_sandbox.unwrap_or_default().into()
    }

    /// The protected-path deny list for write/edit. The default is
    /// pi's protected-paths list; `protectedPaths = []` disables
    /// protection.
    #[must_use]
    pub fn protected_paths(&self) -> Vec<String> {
        self.protected_paths.clone().unwrap_or_else(|| {
            ion_core::PI_PROTECTED_PATHS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        })
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
                provider: "desktop".to_owned(),
                model: "qwen3.8:27b".to_owned(),
            })
        );
        assert_eq!(settings.desktop_base_url(), "http://desktop:8080/v1");
        assert_eq!(settings.theme(), Theme::Auto);
        assert_eq!(settings.thinking_level(), ThinkingLevel::Xhigh);
    }

    #[test]
    fn write_default_model_and_catalog_preserve_other_bytes() {
        let dir = std::env::temp_dir().join(format!("ion-settings-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("settings.toml");
        std::fs::write(
            &path,
            "# maintainer comments stay\ntheme = \"dark\"\n\ndefaultProvider = \"openrouter\"\ndefaultModel = \"old-model\"\n\n[retry]\nenabled = true\n",
        )
        .expect("write");

        let written = Settings::write_default_model(&path, "desktop", "desktop/qwen3.8:27b")
            .expect("write default");
        assert_eq!(written, path);
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains("# maintainer comments stay"), "comments kept");
        assert!(text.contains("theme = \"dark\""), "other key kept");
        assert!(text.contains("defaultProvider = \"desktop\""));
        assert!(
            text.contains("defaultModel = \"qwen3.8:27b\""),
            "bare model"
        );
        assert!(!text.contains("old-model"));

        let written = Settings::write_model_catalog(
            &path,
            &[
                "openrouter/z-ai/glm-5.3-flash".to_owned(),
                "desktop/qwen3.8:27b".to_owned(),
            ],
        )
        .expect("write catalog");
        assert_eq!(written, path);
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains(
            "modelCatalog = [\"openrouter/z-ai/glm-5.3-flash\", \"desktop/qwen3.8:27b\"]"
        ));
        assert!(text.contains("[retry]"), "table kept");

        // An empty catalog removes the key.
        Settings::write_model_catalog(&path, &[]).expect("clear catalog");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(!text.contains("modelCatalog"));
        assert!(text.contains("defaultProvider = \"desktop\""));

        // The result still parses and round-trips the written values.
        let settings: Settings = toml::from_str(&text).expect("written file parses");
        assert_eq!(settings.default_provider.as_deref(), Some("desktop"));
        assert_eq!(settings.default_model.as_deref(), Some("qwen3.8:27b"));
        assert!(settings.model_catalog.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn picker_writes_typed_root_values_and_preserves_nested_keys() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings.toml");
        Settings::write_setting(&path, "theme", "dark").unwrap();
        Settings::write_setting(&path, "hideThinkingBlock", "true").unwrap();
        std::fs::write(&path, "# keep\ntheme= 'dark' # choice\n[other]\ntheme = 'nested'\nmodelCatalog = [\n 'leave',\n]\n").unwrap();
        Settings::write_setting(&path, "theme", "light").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(parsed["theme"].as_str(), Some("light"));
        assert_eq!(parsed["other"]["theme"].as_str(), Some("nested"));
        assert!(text.contains("# keep") && text.contains("# choice"));
        assert!(text.ends_with("[other]\ntheme = 'nested'\nmodelCatalog = [\n 'leave',\n]\n"));
        let corrupt = "theme = invalid[";
        std::fs::write(&path, corrupt).unwrap();
        assert!(Settings::write_setting(&path, "theme", "dark").is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), corrupt);
    }

    #[test]
    fn parses_camel_case_keys_and_qualifies_catalog_entries() {
        let settings: Settings = toml::from_str(
            r#"
            defaultModel = "openrouter/z-ai/glm-5.3-flash"
            defaultProvider = "openrouter"
            modelCatalog = ["z-ai/glm-5.3-flash", "z-ai/glm-5.3-mini"]
            theme = "light"
            "#,
        )
        .unwrap();
        assert_eq!(
            settings.model_selection().unwrap().unwrap().model,
            "z-ai/glm-5.3-flash"
        );
        assert_eq!(
            settings.model_catalog().unwrap(),
            [
                "openrouter/z-ai/glm-5.3-flash",
                "openrouter/z-ai/glm-5.3-mini"
            ]
        );
        assert_eq!(settings.theme(), Theme::Light);
    }

    #[test]
    fn thinking_is_hidden_by_default_and_explicitly_showable() {
        let defaults: Settings = toml::from_str("theme = \"dark\"").unwrap();
        assert!(defaults.hide_thinking_block);
        let empty: Settings = toml::from_str("").unwrap();
        assert!(empty.hide_thinking_block);

        let shown: Settings = toml::from_str("hideThinkingBlock = false").unwrap();
        assert!(!shown.hide_thinking_block);
    }

    #[test]
    fn agent_tools_are_disabled_by_default_and_explicitly_enableable() {
        let defaults: Settings = toml::from_str("theme = \"dark\"").unwrap();
        assert!(!defaults.agents_enabled());

        let enabled: Settings = toml::from_str("enableAgents = true").unwrap();
        assert!(enabled.agents_enabled());
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
    fn desktop_provider_and_endpoint_are_supported() {
        let settings: Settings = toml::from_str(
            r#"
            defaultModel = "desktop/qwen3.8:27b"
            defaultProvider = "desktop"
            desktopBaseUrl = "http://127.0.0.1:9000/v1"
            desktopApiKey = "local-only"
            "#,
        )
        .unwrap();
        assert_eq!(
            settings.model_selection().unwrap(),
            Some(ModelSelection {
                provider: "desktop".to_owned(),
                model: "qwen3.8:27b".to_owned(),
            })
        );
        assert_eq!(settings.desktop_base_url(), "http://127.0.0.1:9000/v1");
        assert_eq!(settings.desktop_api_key.as_deref(), Some("local-only"));
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
pub fn load_extension_defs(
    settings: &Settings,
    project_root: Option<&std::path::Path>,
    trust_project: bool,
) -> Result<Vec<ion_core::ExtensionDef>, String> {
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
        return Ok(defs);
    };
    let path = root.join(".ion").join("extensions.toml");
    if !trust_project {
        if path.exists() {
            tracing::warn!(path = %path.display(), "project extensions ignored: workspace is not trusted");
        }
        return Ok(defs);
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(defs),
        Err(err) => return Err(format!("project extensions {}: {err}", path.display())),
    };
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
        Err(err) => return Err(format!("project extensions {}: {err}", path.display())),
    }
    Ok(defs)
}

#[derive(Debug, Deserialize)]
struct ProjectExtensions {
    #[serde(default)]
    extensions: Vec<ExtensionConfig>,
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn retry_defaults_match_pi() {
        let policy = Settings::empty().retry_policy();
        assert!(policy.enabled);
        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.base_delay_ms, 2000);
    }

    #[test]
    fn retry_settings_override_and_disable() {
        let settings: Settings = toml::from_str(
            r#"
            [retry]
            enabled = false
            max_retries = 5
            base_delay_ms = 500
            "#,
        )
        .expect("parse retry table");
        let policy = settings.retry_policy();
        assert!(!policy.enabled);
        assert_eq!(policy.max_retries, 5);
        assert_eq!(policy.base_delay_ms, 500);
    }
}

#[cfg(test)]
mod workspace_sandbox_tests {
    use super::*;

    #[test]
    fn workspace_sandbox_defaults_to_pi_parity_off() {
        assert_eq!(
            Settings::empty().workspace_policy(),
            ion_core::WorkspacePolicy::Unrestricted
        );
    }

    #[test]
    fn workspace_sandbox_workspace_confines() {
        let settings: Settings = toml::from_str(
            r#"
            workspaceSandbox = "workspace"
            "#,
        )
        .expect("parse workspaceSandbox");
        assert_eq!(
            settings.workspace_policy(),
            ion_core::WorkspacePolicy::Confined
        );
    }
}

#[cfg(test)]
mod protected_paths_settings_tests {
    use super::*;

    #[test]
    fn protected_paths_default_to_pi_extension_list() {
        let paths = Settings::empty().protected_paths();
        assert_eq!(
            paths,
            vec![
                ".env".to_string(),
                ".env.".to_string(),
                ".git/".to_string(),
                "node_modules/".to_string(),
                ".chezmoidata.yaml".to_string(),
                ".chezmoidata.yaml.age".to_string(),
                ".config/age/keys.txt".to_string(),
            ]
        );
    }

    #[test]
    fn protected_paths_setting_overrides_and_empty_disables() {
        let custom: Settings =
            toml::from_str(r#"protectedPaths = [".env", "secrets/"]"#).expect("parse custom");
        assert_eq!(custom.protected_paths(), vec![".env", "secrets/"]);
        let disabled: Settings = toml::from_str(r#"protectedPaths = []"#).expect("parse empty");
        assert!(disabled.protected_paths().is_empty());
    }
}

/// Canonical structural peer definitions for durable authority. Inactive MCP
/// servers retain supervision but receive no grant; selecting them again is a
/// new admission. Validate every definition before any host mutation.
pub fn peer_authority_definitions(
    mcp: &[ion_core::ServerDef],
    extensions: &[ion_core::ExtensionDef],
    active_mcp: &[String],
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut desired = std::collections::BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    for (prefix, name, command, args, active) in mcp
        .iter()
        .map(|def| {
            (
                "mcp",
                &def.name,
                &def.command,
                &def.args,
                active_mcp.contains(&def.name),
            )
        })
        .chain(
            extensions
                .iter()
                .map(|def| ("ext", &def.name, &def.command, &def.args, true)),
        )
    {
        let scope = format!("{prefix}:{name}");
        if name.trim().is_empty()
            || command.trim().is_empty()
            || name.contains('\0')
            || command.contains('\0')
            || args.iter().any(|arg| arg.contains('\0'))
        {
            return Err(format!("invalid peer definition: {scope}"));
        }
        if !seen.insert(scope.clone()) {
            return Err(format!("duplicate peer: {scope}"));
        }
        if active {
            let definition =
                serde_json::to_string(&(command, args)).map_err(|err| err.to_string())?;
            desired.insert(scope, definition);
        }
    }
    Ok(desired)
}
