//! Host-owned model routes and the default choice. Sessions store only model identity.

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use ion_ai::ModelRef;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

mod cycling;
mod matching;
mod scope;
#[cfg(test)]
mod scope_tests;

pub use cycling::ModelCycleDirection;

use crate::{
    HttpModelService, HttpWire,
    auth::{CredentialStatus, CredentialStore, validate_provider_id},
    catalog,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wire {
    ChatCompletions,
    #[serde(rename = "openrouter-chat")]
    OpenRouterChat,
    LlamaCppNoThinking,
    AnthropicMessages,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
}

impl Wire {
    pub const VALUES: &[(&str, Self)] = &[
        ("chat-completions", Self::ChatCompletions),
        ("openrouter-chat", Self::OpenRouterChat),
        ("llama-cpp-no-thinking", Self::LlamaCppNoThinking),
        ("anthropic-messages", Self::AnthropicMessages),
        ("openai-responses", Self::OpenAiResponses),
    ];
}

impl std::str::FromStr for Wire {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Self::VALUES
            .iter()
            .find_map(|(name, wire)| (*name == value).then_some(*wire))
            .ok_or_else(|| format!("unknown wire format: {value}"))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedSelection {
    pub provider: String,
    pub model: String,
    pub endpoint: Option<String>,
    pub wire: Option<Wire>,
    pub api_key_env: Option<String>,
    /// Explicit custom-model capacity; absent means unknown, not a guessed window.
    pub context_window_tokens: Option<u32>,
    /// Custom-model output ceiling; absent retains the default ceiling.
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub image_input: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelConfiguration {
    custom: Vec<SavedSelection>,
    default: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct Selection {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub wire: HttpWire,
    pub api_key_env: Option<String>,
    pub max_output_tokens: u32,
    pub context_window_tokens: Option<u32>,
    pub requires_key: bool,
    pub image_input: bool,
    pub capabilities: catalog::ModelCapabilities,
}

/// No explicit, restored, saved or authenticated catalog selection is available.
/// Interactive clients may offer setup; unattended callers must report the error.
#[derive(Debug)]
pub struct ModelSelectionRequired;

impl std::fmt::Display for ModelSelectionRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "no model selected; run `ion models`, then `ion use PROVIDER MODEL` or set a provider key",
        )
    }
}

impl std::error::Error for ModelSelectionRequired {}

impl Selection {
    pub fn identity(&self) -> ModelRef {
        ModelRef {
            provider: self.provider.clone(),
            model: self.model.clone(),
        }
    }

    pub fn credential_missing(&self, credentials: &CredentialStore) -> Result<bool> {
        Ok(self.requires_key
            && credentials.status(&self.provider, self.api_key_env.as_deref())?
                == CredentialStatus::Missing)
    }

    pub fn require_access(&self, credentials: &CredentialStore) -> Result<()> {
        if self.credential_missing(credentials)? {
            if let Some(env_name) = &self.api_key_env {
                bail!(
                    "no {} credential; set {} or run `ion login {}`",
                    self.provider,
                    env_name,
                    self.provider
                );
            }
            bail!(
                "no {provider} credential; run `ion login {provider}`",
                provider = self.provider
            );
        }
        Ok(())
    }
}

pub struct ModelChoice {
    pub selected: Selection,
    pub label: &'static str,
    pub credential: CredentialStatus,
}

impl ModelChoice {
    /// Credential presence is local metadata, not remote authentication.
    pub fn is_ready(&self) -> bool {
        !self.selected.requires_key || self.credential != CredentialStatus::Missing
    }
}

pub struct ModelStore {
    root: PathBuf,
    scope_override: Option<Vec<String>>,
}

impl ModelStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            scope_override: None,
        }
    }

    /// Invocation-local preferences; an empty list explicitly disables a saved scope.
    pub fn with_scope(mut self, patterns: Vec<String>) -> Self {
        self.scope_override = Some(patterns);
        self
    }

    pub fn save_default(&self, saved: &SavedSelection) -> Result<Selection> {
        validate_provider_id(&saved.provider)?;
        // Serialize read/modify/publish, including between separate CLI processes.
        // Keep the lock inode stable while replacing the configuration document.
        let _lock = self.writer_lock()?;
        let mut configuration = self.configuration()?;
        let effective =
            if saved.endpoint.is_none() && catalog::find(&saved.provider, &saved.model).is_none() {
                ensure!(
                    saved.wire.is_none()
                        && saved.api_key_env.is_none()
                        && !saved.image_input
                        && saved.context_window_tokens.is_none()
                        && saved.max_output_tokens.is_none(),
                    "custom route overrides require --endpoint and --wire"
                );
                configuration
                    .custom
                    .iter()
                    .find(|route| route.provider == saved.provider && route.model == saved.model)
                    .context("custom model has no configured route")?
                    .clone()
            } else {
                saved.clone()
            };
        let selected = self.resolve_saved(&effective)?;
        if effective.endpoint.is_some() {
            configuration.custom.retain(|route| {
                route.provider != effective.provider || route.model != effective.model
            });
            configuration.custom.push(effective);
        }
        configuration.default = Some(selected.identity());
        if let Some(scope) = configuration
            .scope
            .as_mut()
            .filter(|scope| !scope.is_empty())
        {
            // An explicitly saved default must remain in the saved rotation even
            // when aliases/catalogues change. Overlaps deduplicate on resolution.
            let canonical = format!("{}/{}", selected.provider, selected.model);
            if !scope
                .iter()
                .any(|pattern| pattern.to_lowercase() == canonical.to_lowercase())
            {
                scope.push(canonical);
            }
        }
        write_json(&self.root.join("models.json"), &configuration)?;
        Ok(selected)
    }

    pub fn choose(
        &self,
        explicit: Option<ModelRef>,
        previous: Option<ModelRef>,
        credentials: &CredentialStore,
    ) -> Result<Selection> {
        if let Some(explicit) = explicit {
            return self.resolve_identity(&explicit);
        }
        if let Some(previous) = previous {
            return self.resolve_identity(&previous).with_context(|| {
                format!(
                    "saved session model {}/{} is unavailable; select another model explicitly",
                    previous.provider, previous.model
                )
            });
        }
        let configuration = self.configuration()?;
        if self.scope(&configuration).is_some() {
            let choices = self.cycle_choices_with(&configuration, credentials)?;
            let mut ready = choices.into_iter().filter(ModelChoice::is_ready);
            let first = ready.next();
            let preferred = configuration
                .default
                .as_ref()
                .and_then(|default| ready.find(|choice| choice.selected.identity() == *default));
            return preferred
                .or(first)
                .map(|choice| choice.selected)
                .ok_or_else(|| ModelSelectionRequired.into());
        }
        if let Some(selected) = &configuration.default {
            return self.resolve_identity_with(selected, &configuration);
        }
        for entry in catalog::models() {
            if credentials.status(entry.provider, Some(entry.api_key_env))?
                != CredentialStatus::Missing
            {
                return self.resolve_identity(&ModelRef {
                    provider: entry.provider.into(),
                    model: entry.id.into(),
                });
            }
        }
        Err(ModelSelectionRequired.into())
    }

    /// Resolve human shorthand once, before binding. Never use this to restore
    /// a stored identity or interpret a new endpoint's literal model ID.
    pub fn resolve_pattern(
        &self,
        text: &str,
        provider: Option<&str>,
        credentials: &CredentialStore,
    ) -> Result<Selection> {
        let text = text.trim();
        ensure!(!text.is_empty(), "model pattern must not be empty");
        if let Some(provider) = provider {
            validate_provider_id(&provider.to_ascii_lowercase())?;
        }
        let scoped = provider
            .map(|provider| (provider, text))
            .or_else(|| text.split_once('/'));
        if let Some(entry) = scoped.and_then(|(provider, id)| {
            catalog::models()
                .iter()
                .find(|entry| entry.provider.eq_ignore_ascii_case(provider) && entry.id == id)
        }) {
            // Preserve exact catalog lookup even if unrelated saved routes or
            // credentials are broken. Do not case-fold an exact custom ID away.
            return self.resolve_identity(&ModelRef {
                provider: entry.provider.into(),
                model: entry.id.into(),
            });
        }
        let configuration = self.configuration()?;
        let candidates = catalog::models()
            .iter()
            .map(|entry| matching::Candidate {
                provider: entry.provider,
                id: entry.id,
                name: entry.label,
            })
            .chain(
                configuration
                    .custom
                    .iter()
                    .map(|route| matching::Candidate {
                        provider: &route.provider,
                        id: &route.model,
                        name: &route.model,
                    }),
            )
            .collect::<Vec<_>>();
        let identity = |candidate: matching::Candidate<'_>| ModelRef {
            provider: candidate.provider.into(),
            model: candidate.id.into(),
        };
        let selected = matching::resolve(&candidates, text, provider, |candidate| {
            Ok(!self
                .resolve_identity_with(&identity(candidate), &configuration)?
                .credential_missing(credentials)?)
        })?;
        self.resolve_identity_with(&identity(selected), &configuration)
    }

    pub fn resolve_identity(&self, model: &ModelRef) -> Result<Selection> {
        validate_provider_id(&model.provider)?;
        let configuration = if catalog::find(&model.provider, &model.model).is_some() {
            // Explicit catalog selection is independent of saved custom routes.
            ModelConfiguration::default()
        } else {
            self.configuration()?
        };
        self.resolve_identity_with(model, &configuration)
    }

    fn resolve_identity_with(
        &self,
        model: &ModelRef,
        configuration: &ModelConfiguration,
    ) -> Result<Selection> {
        validate_provider_id(&model.provider)?;
        if catalog::find(&model.provider, &model.model).is_some() {
            return self.resolve_saved(&SavedSelection {
                provider: model.provider.clone(),
                model: model.model.clone(),
                endpoint: None,
                wire: None,
                api_key_env: None,
                context_window_tokens: None,
                max_output_tokens: None,
                image_input: false,
            });
        }
        if let Some(route) = configuration
            .custom
            .iter()
            .find(|route| route.provider == model.provider && route.model == model.model)
        {
            return self.resolve_saved(route);
        }
        bail!("no configured route for {}/{}", model.provider, model.model)
    }

    pub fn options(&self) -> Result<Vec<Selection>> {
        self.options_with(&self.configuration()?)
    }

    fn options_with(&self, configuration: &ModelConfiguration) -> Result<Vec<Selection>> {
        let mut options = catalog::models()
            .iter()
            .map(|entry| {
                self.resolve_identity_with(
                    &ModelRef {
                        provider: entry.provider.into(),
                        model: entry.id.into(),
                    },
                    configuration,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        for route in &configuration.custom {
            options.push(self.resolve_saved(route)?);
        }
        Ok(options)
    }

    pub fn choices(&self, credentials: &CredentialStore) -> Result<Vec<ModelChoice>> {
        self.choices_with(&self.configuration()?, credentials)
    }

    /// Ordered scope choices for cycling. Missing keys are retained as metadata;
    /// the cycling policy excludes them, without widening an unavailable scope.
    pub fn cycle_choices(&self, credentials: &CredentialStore) -> Result<Vec<ModelChoice>> {
        self.cycle_choices_with(&self.configuration()?, credentials)
    }

    fn scope<'a>(&'a self, configuration: &'a ModelConfiguration) -> Option<&'a [String]> {
        self.scope_override
            .as_ref()
            .or(configuration.scope.as_ref())
            .filter(|patterns| !patterns.is_empty())
            .map(Vec::as_slice)
    }

    fn cycle_choices_with(
        &self,
        configuration: &ModelConfiguration,
        credentials: &CredentialStore,
    ) -> Result<Vec<ModelChoice>> {
        let choices = self.choices_with(configuration, credentials)?;
        match self.scope(configuration) {
            Some(patterns) => scope::resolve(patterns, choices),
            None => Ok(choices),
        }
    }

    fn choices_with(
        &self,
        configuration: &ModelConfiguration,
        credentials: &CredentialStore,
    ) -> Result<Vec<ModelChoice>> {
        self.options_with(configuration)?
            .into_iter()
            .map(|selected| {
                let label = catalog::find(&selected.provider, &selected.model)
                    .map_or("Custom endpoint", |entry| entry.label);
                let credential =
                    credentials.status(&selected.provider, selected.api_key_env.as_deref())?;
                Ok(ModelChoice {
                    selected,
                    label,
                    credential,
                })
            })
            .collect()
    }

    fn configuration(&self) -> Result<ModelConfiguration> {
        Ok(read_json(&self.root.join("models.json"))?.unwrap_or_default())
    }

    fn writer_lock(&self) -> Result<File> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)?;
        let lock = File::from(rustix::fs::open(
            self.root.join("models.lock"),
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )?);
        ensure!(
            lock.metadata()?.is_file(),
            "model lock is not a regular file"
        );
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
        Ok(lock)
    }

    fn resolve_saved(&self, saved: &SavedSelection) -> Result<Selection> {
        validate_provider_id(&saved.provider)?;
        ensure!(
            saved.context_window_tokens != Some(0),
            "context window must be greater than zero"
        );
        ensure!(
            saved.max_output_tokens != Some(0),
            "maximum output tokens must be greater than zero"
        );
        if let Some(model) = catalog::find(&saved.provider, &saved.model) {
            ensure!(
                saved.endpoint.is_none()
                    && saved.wire.is_none()
                    && saved.api_key_env.is_none()
                    && !saved.image_input
                    && saved.context_window_tokens.is_none()
                    && saved.max_output_tokens.is_none(),
                "catalog model route cannot be overridden; use a custom provider identifier"
            );
            return Ok(Selection {
                provider: saved.provider.clone(),
                model: saved.model.clone(),
                endpoint: model.endpoint.into(),
                wire: match model.wire {
                    catalog::CatalogWire::OpenAiResponses => HttpWire::OpenAiResponses,
                    catalog::CatalogWire::DeepSeekChat => HttpWire::DeepSeekChat,
                    catalog::CatalogWire::MiMoChat => HttpWire::MiMoChat,
                    catalog::CatalogWire::OpenRouterChat => HttpWire::OpenRouterChat,
                    catalog::CatalogWire::AnthropicMessages => HttpWire::AnthropicMessages,
                },
                api_key_env: Some(model.api_key_env.into()),
                max_output_tokens: model.max_output_tokens,
                context_window_tokens: Some(model.context_window),
                requires_key: true,
                image_input: model.image_input,
                capabilities: model.capabilities,
            });
        }
        let endpoint = saved
            .endpoint
            .as_deref()
            .context("unknown model; supply --endpoint and --wire to configure a custom route")?;
        let wire = saved.wire.context("custom route requires --wire")?;
        let wire = match wire {
            Wire::ChatCompletions => HttpWire::ChatCompletions,
            Wire::OpenRouterChat => HttpWire::OpenRouterChat,
            Wire::LlamaCppNoThinking => HttpWire::LlamaCppNoThinking,
            Wire::AnthropicMessages => HttpWire::AnthropicMessages,
            Wire::OpenAiResponses => HttpWire::OpenAiResponses,
        };
        let endpoint = HttpModelService::resolve_endpoint(endpoint, wire)
            .context("invalid custom endpoint")?;
        ensure!(!saved.model.is_empty(), "model ID is empty");
        ensure!(
            saved.api_key_env.as_deref() != Some(""),
            "credential environment name is empty"
        );
        Ok(Selection {
            provider: saved.provider.clone(),
            model: saved.model.clone(),
            endpoint,
            wire,
            api_key_env: saved.api_key_env.clone(),
            max_output_tokens: saved.max_output_tokens.unwrap_or(8192),
            context_window_tokens: saved.context_window_tokens,
            requires_key: false,
            image_input: saved.image_input,
            capabilities: catalog::ModelCapabilities::conservative(),
        })
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match crate::file_io::open_regular(path) {
        Ok(file) => Ok(Some(
            serde_json::from_reader(io::BufReader::new(file))
                .with_context(|| format!("invalid {}", path.display()))?,
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_json_with_mode(path, value, 0o600)
}

/// Project files are ordinary repository files, not private credentials.
pub(crate) fn write_shared_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_json_with_mode(path, value, 0o644)
}

fn write_json_with_mode<T: Serialize>(path: &Path, value: &T, mode: u32) -> Result<()> {
    let parent = path.parent().context("config path has no parent")?;
    if !parent.exists() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow::anyhow!("cannot generate config temp name: {error}"))?;
    let temp = path.with_extension(format!(
        "{}.{:016x}.tmp",
        std::process::id(),
        u64::from_ne_bytes(nonce)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| {
                format!(
                    "published {}, but syncing its directory failed",
                    path.display()
                )
            })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop_route(endpoint: &str) -> SavedSelection {
        SavedSelection {
            provider: "desktop".into(),
            model: "qwen".into(),
            endpoint: Some(endpoint.into()),
            wire: Some(Wire::ChatCompletions),
            api_key_env: None,
            context_window_tokens: None,
            max_output_tokens: None,
            image_input: false,
        }
    }

    #[test]
    fn custom_route_uses_transport_endpoint_policy() {
        let root = std::env::temp_dir().join(format!("ion-endpoint-policy-{}", std::process::id()));
        let store = ModelStore::new(root);
        let mut route = SavedSelection {
            provider: "desktop".into(),
            model: "local".into(),
            endpoint: Some("http://localhost:1234/v1/chat/completions".into()),
            wire: Some(Wire::ChatCompletions),
            api_key_env: None,
            context_window_tokens: None,
            max_output_tokens: None,
            image_input: false,
        };
        assert!(!store.resolve_saved(&route).unwrap().requires_key);
        route.wire = Some("openrouter-chat".parse().unwrap());
        assert_eq!(
            serde_json::to_string(&route.wire).unwrap(),
            "\"openrouter-chat\""
        );
        assert!(matches!(
            serde_json::from_str::<Option<Wire>>("\"openrouter-chat\"").unwrap(),
            Some(Wire::OpenRouterChat)
        ));
        assert_eq!(
            store.resolve_saved(&route).unwrap().wire,
            HttpWire::OpenRouterChat
        );
        route.endpoint = Some("https://localhost/v1/chat/completions".into());
        assert!(!store.resolve_saved(&route).unwrap().requires_key);
        route.endpoint = Some("http://desktop:8080/v1/chat/completions".into());
        assert!(!store.resolve_saved(&route).unwrap().requires_key);
        route.endpoint = Some("https://example.com/v1/chat/completions".into());
        assert!(!store.resolve_saved(&route).unwrap().requires_key);
        route.endpoint = Some("https://example.com/v1/chat/completions?token=hidden".into());
        assert!(store.resolve_saved(&route).is_err());
    }

    #[test]
    fn responses_custom_route_round_trips() {
        let store = ModelStore::new(std::env::temp_dir().join("ion-responses-route"));
        let mut route = desktop_route("https://api.example.com/prefix/v1/");
        route.wire = Some("openai-responses".parse().unwrap());
        let serialized = serde_json::to_string(&route).unwrap();
        assert!(serialized.contains("\"openai-responses\""));
        let route: SavedSelection = serde_json::from_str(&serialized).unwrap();
        let selected = store.resolve_saved(&route).unwrap();
        assert_eq!(selected.wire, HttpWire::OpenAiResponses);
        assert_eq!(
            selected.endpoint,
            "https://api.example.com/prefix/v1/responses"
        );
        assert_eq!(
            HttpModelService::resolve_endpoint(&selected.endpoint, selected.wire).unwrap(),
            selected.endpoint
        );
    }

    #[test]
    fn known_openai_selection_uses_responses_without_route_configuration() {
        let root =
            std::env::temp_dir().join(format!("ion-openai-default-{}", uuid::Uuid::now_v7()));
        let store = ModelStore::new(root.clone());
        let credentials = CredentialStore::new(root.join("credentials"));
        for (model, context_window) in [
            ("gpt-5.5", 1_050_000),
            ("gpt-5.4-mini", 400_000),
            ("gpt-5.4", 1_050_000),
        ] {
            let saved = SavedSelection {
                provider: "openai".into(),
                model: model.into(),
                endpoint: None,
                wire: None,
                api_key_env: None,
                context_window_tokens: None,
                max_output_tokens: None,
                image_input: false,
            };
            let selected = store.save_default(&saved).unwrap();
            let reopened = ModelStore::new(root.clone());
            for selected in [
                selected,
                reopened
                    .choose(
                        Some(ModelRef {
                            provider: "openai".into(),
                            model: model.into(),
                        }),
                        None,
                        &credentials,
                    )
                    .unwrap(),
                reopened.choose(None, None, &credentials).unwrap(),
                reopened
                    .choose(
                        None,
                        Some(ModelRef {
                            provider: "openai".into(),
                            model: model.into(),
                        }),
                        &credentials,
                    )
                    .unwrap(),
            ] {
                assert_eq!(selected.wire, HttpWire::OpenAiResponses);
                assert_eq!(selected.endpoint, "https://api.openai.com/v1/responses");
                assert_eq!(selected.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
                assert!(selected.requires_key && selected.image_input);
                assert_eq!(selected.context_window_tokens, Some(context_window));
                assert_eq!(selected.max_output_tokens, 128_000);
                assert!(selected.capabilities.prompt_cache.usage_details);
            }
        }
        assert!(store.configuration().unwrap().custom.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn human_patterns_resolve_canonical_routes_without_rewriting_exact_state() {
        let root = std::env::temp_dir().join(format!("ion-model-pattern-{}", uuid::Uuid::now_v7()));
        let store = ModelStore::new(root.clone());
        let credentials = CredentialStore::new(root.join("credentials"));
        let mut route = desktop_route("http://localhost:8080/v1");
        route.model = "vendor/alpha-latest:high".into();
        let exact = store.save_default(&route).unwrap().identity();
        let publication = root.join("models.json");
        let before = fs::read(&publication).unwrap();
        for (text, provider) in [
            ("alpha", None),
            ("DESKTOP/ALPHA", None),
            ("vendor/alpha-latest:high", Some("desktop")),
        ] {
            assert_eq!(
                store
                    .resolve_pattern(text, provider, &credentials)
                    .unwrap()
                    .identity(),
                exact
            );
        }
        assert_eq!(fs::read(&publication).unwrap(), before);
        assert!(
            store
                .resolve_identity(&ModelRef {
                    provider: "desktop".into(),
                    model: "alpha".into()
                })
                .is_err()
        );
        assert!(
            store
                .choose(
                    None,
                    Some(ModelRef {
                        provider: "desktop".into(),
                        model: "alpha".into()
                    }),
                    &credentials
                )
                .is_err()
        );
        let mut configuration: ModelConfiguration = serde_json::from_slice(&before).unwrap();
        configuration.custom.clear();
        write_json(&publication, &configuration).unwrap();
        assert!(store.choose(None, None, &credentials).is_err());
        fs::write(&publication, b"not JSON").unwrap();
        assert_eq!(
            store
                .resolve_pattern("openai/gpt-5.5", None, &credentials)
                .unwrap()
                .model,
            "gpt-5.5"
        );
        assert_eq!(
            store
                .resolve_pattern("gpt-5.5", Some("openai"), &credentials)
                .unwrap()
                .model,
            "gpt-5.5"
        );
        assert!(
            store
                .resolve_pattern("gpt", Some("openai"), &credentials)
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catalog_capabilities_are_explicit_and_custom_routes_are_conservative() {
        let root =
            std::env::temp_dir().join(format!("ion-model-capabilities-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = ModelStore::new(root.clone());

        let claude = store
            .resolve_identity(&ModelRef {
                provider: "anthropic".into(),
                model: "claude-opus-5-5".into(),
            })
            .unwrap();
        let openrouter = store
            .resolve_identity(&ModelRef {
                provider: "openrouter".into(),
                model: "openai/gpt-5.4".into(),
            })
            .unwrap();
        assert_eq!(openrouter.wire, HttpWire::OpenRouterChat);
        assert!(claude.capabilities.context_mutation.mid_conversation_system);
        assert!(claude.capabilities.context_mutation.mid_conversation_tools);
        assert_eq!(
            claude.capabilities.prompt_cache.lifetime,
            catalog::PromptCacheLifetime::Fixed {
                default_seconds: 300,
                extended_seconds: Some(3600),
            }
        );

        let custom = store
            .resolve_saved(&SavedSelection {
                provider: "custom".into(),
                model: "unknown".into(),
                endpoint: Some("https://example.com/v1/chat/completions".into()),
                wire: Some(Wire::ChatCompletions),
                api_key_env: None,
                context_window_tokens: None,
                max_output_tokens: None,
                image_input: false,
            })
            .unwrap();
        assert_eq!(
            custom.capabilities,
            catalog::ModelCapabilities::conservative()
        );
        assert_eq!(custom.context_window_tokens, None);
        assert_eq!(custom.max_output_tokens, 8192);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_capacity_overrides_preserve_the_configured_route() {
        let root = std::env::temp_dir().join(format!("ion-model-limits-{}", uuid::Uuid::now_v7()));
        let store = ModelStore::new(root.clone());
        let route = desktop_route("http://localhost:8080/v1");
        store.save_default(&route).unwrap();
        let publication = root.join("models.json");
        let before = fs::read(&publication).unwrap();
        let catalog = SavedSelection {
            provider: "anthropic".into(),
            model: "claude-opus-5-5".into(),
            endpoint: None,
            wire: None,
            ..route.clone()
        };
        for invalid in [
            SavedSelection {
                context_window_tokens: Some(0),
                ..route.clone()
            },
            SavedSelection {
                max_output_tokens: Some(0),
                ..route.clone()
            },
            SavedSelection {
                context_window_tokens: Some(50_000),
                ..catalog.clone()
            },
            SavedSelection {
                max_output_tokens: Some(1234),
                ..catalog
            },
            SavedSelection {
                endpoint: None,
                wire: None,
                context_window_tokens: Some(50_000),
                ..route.clone()
            },
            SavedSelection {
                endpoint: None,
                wire: None,
                max_output_tokens: Some(1234),
                ..route.clone()
            },
        ] {
            assert!(store.save_default(&invalid).is_err());
            assert_eq!(fs::read(&publication).unwrap(), before);
        }
        let mut configuration: serde_json::Value = serde_json::from_slice(&before).unwrap();
        configuration["custom"][0]["context_window_token"] = 50_000.into();
        fs::write(&publication, serde_json::to_vec(&configuration).unwrap()).unwrap();
        assert!(
            store
                .resolve_identity(&ModelRef {
                    provider: route.provider,
                    model: route.model,
                })
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_default_publication_preserves_the_previous_custom_route() {
        let root = std::env::temp_dir().join(format!("ion-model-atomic-{}", uuid::Uuid::now_v7()));
        let store = ModelStore::new(root.clone());
        let mut route = desktop_route("http://localhost:8080/v1");
        let first = store.save_default(&route).unwrap();
        let publication = root.join("models.json");
        let backup = root.join("backup.json");
        fs::rename(&publication, &backup).unwrap();
        fs::create_dir(&publication).unwrap();
        route.endpoint = Some("http://localhost:9090/v1".into());
        assert!(store.save_default(&route).is_err());
        fs::remove_dir(&publication).unwrap();
        fs::rename(&backup, &publication).unwrap();
        let reopened = ModelStore::new(root.clone());
        assert_eq!(
            reopened
                .resolve_identity(&first.identity())
                .unwrap()
                .endpoint,
            first.endpoint
        );
        assert_eq!(
            reopened
                .choose(None, None, &CredentialStore::new(root.join("credentials")))
                .unwrap()
                .endpoint,
            first.endpoint
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_provider_ids_cannot_replace_a_usable_default() {
        let root = std::env::temp_dir().join(format!("ion-model-id-{}", uuid::Uuid::now_v7()));
        let store = ModelStore::new(root.clone());
        let mut route = desktop_route("http://localhost:8080/v1");
        let first = store.save_default(&route).unwrap();
        let too_long = "a".repeat(65);
        for provider in ["", "Desktop", "bad/name", "café", too_long.as_str()] {
            route.provider = provider.into();
            assert!(store.save_default(&route).is_err(), "accepted {provider:?}");
            assert!(store.resolve_saved(&route).is_err());
            let credentials = CredentialStore::new(root.join("credentials"));
            assert_eq!(
                credentials
                    .status(provider, Some(""))
                    .unwrap_err()
                    .to_string(),
                "invalid provider identifier"
            );
            assert!(credentials.resolver(provider, None).is_err());
            assert_eq!(
                store
                    .choose(None, None, &CredentialStore::new(root.join("credentials")))
                    .unwrap()
                    .identity(),
                first.identity()
            );
        }
        assert!(!root.join("credentials").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_model_saves_retain_each_route_and_a_resolvable_default() {
        let root = std::env::temp_dir().join(format!("ion-model-writers-{}", uuid::Uuid::now_v7()));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let writers = (0..8)
            .map(|index| {
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let route = SavedSelection {
                        model: format!("local-{index}"),
                        ..desktop_route(&format!("http://localhost:{}/v1", 8080 + index))
                    };
                    barrier.wait();
                    ModelStore::new(root).save_default(&route).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let saved = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect::<Vec<_>>();
        let store = ModelStore::new(root.clone());
        for selected in &saved {
            assert_eq!(
                store
                    .resolve_identity(&selected.identity())
                    .unwrap()
                    .endpoint,
                selected.endpoint
            );
        }
        let default = store
            .choose(None, None, &CredentialStore::new(root.join("credentials")))
            .unwrap();
        assert!(saved.iter().any(|selected| {
            selected.identity() == default.identity() && selected.endpoint == default.endpoint
        }));
        assert_eq!(store.configuration().unwrap().custom.len(), saved.len());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resumed_custom_route_survives_a_new_global_default() {
        let root = std::env::temp_dir().join(format!("ion-model-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = ModelStore::new(root.clone());
        let custom: SavedSelection = serde_json::from_value(serde_json::json!({
            "provider": "desktop",
            "model": "qwen",
            "endpoint": "http://127.0.0.1:8080/v1/chat/completions",
            "wire": "llama-cpp-no-thinking",
            "api_key_env": null,
            "image_input": false,
            "context_window_tokens": 50_000,
            "max_output_tokens": 1234,
        }))
        .unwrap();
        store.save_default(&custom).unwrap();
        store
            .save_default(&SavedSelection {
                provider: "desktop".into(),
                model: "qwen".into(),
                endpoint: None,
                wire: None,
                api_key_env: None,
                context_window_tokens: None,
                max_output_tokens: None,
                image_input: false,
            })
            .unwrap();
        store
            .save_default(&SavedSelection {
                provider: "openrouter".into(),
                model: "deepseek/deepseek-v4.1-flash".into(),
                endpoint: None,
                wire: None,
                api_key_env: None,
                context_window_tokens: None,
                max_output_tokens: None,
                image_input: false,
            })
            .unwrap();
        let credentials = CredentialStore::new(root.join("credentials"));
        let store = ModelStore::new(root.clone());
        let selected = store
            .choose(
                None,
                Some(ModelRef {
                    provider: "desktop".into(),
                    model: "qwen".into(),
                }),
                &credentials,
            )
            .unwrap();
        assert_eq!(
            selected.endpoint,
            "http://127.0.0.1:8080/v1/chat/completions"
        );
        assert!(
            store
                .options()
                .unwrap()
                .iter()
                .any(|option| option.provider == "desktop")
        );
        fs::remove_dir_all(root).unwrap();
        assert_eq!(selected.context_window_tokens, Some(50_000));
        assert_eq!(selected.max_output_tokens, 1234);
    }
}
