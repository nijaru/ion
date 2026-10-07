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
}

impl std::str::FromStr for Wire {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "chat-completions" => Ok(Self::ChatCompletions),
            "openrouter-chat" => Ok(Self::OpenRouterChat),
            "llama-cpp-no-thinking" => Ok(Self::LlamaCppNoThinking),
            "anthropic-messages" => Ok(Self::AnthropicMessages),
            _ => Err(format!("unknown wire format: {value}")),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedSelection {
    pub provider: String,
    pub model: String,
    pub endpoint: Option<String>,
    pub wire: Option<Wire>,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub image_input: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelConfiguration {
    custom: Vec<SavedSelection>,
    default: Option<ModelRef>,
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

impl Selection {
    pub fn identity(&self) -> ModelRef {
        ModelRef {
            provider: self.provider.clone(),
            model: self.model.clone(),
        }
    }

    pub fn require_access(&self, credentials: &CredentialStore) -> Result<()> {
        if self.requires_key
            && credentials.status(&self.provider, self.api_key_env.as_deref())?
                == CredentialStatus::Missing
        {
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

pub struct ModelStore {
    root: PathBuf,
}

impl ModelStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
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
                    saved.wire.is_none() && saved.api_key_env.is_none() && !saved.image_input,
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
        write_json(&self.root.join("models.json"), &configuration)?;
        Ok(selected)
    }

    pub fn choose(
        &self,
        provider: Option<String>,
        model: Option<String>,
        previous: Option<ModelRef>,
        credentials: &CredentialStore,
    ) -> Result<Selection> {
        if provider.is_some() != model.is_some() {
            bail!("--provider and --model must be used together");
        }
        if let (Some(provider), Some(model)) = (provider, model) {
            return self.resolve_identity(&ModelRef { provider, model });
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
        bail!(
            "no model selected; run `ion models`, then `ion use PROVIDER MODEL` or set a provider key"
        )
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
        let mut options = catalog::models()
            .iter()
            .map(|entry| {
                self.resolve_identity(&ModelRef {
                    provider: entry.provider.into(),
                    model: entry.id.into(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        for route in self.configuration()?.custom {
            options.push(self.resolve_saved(&route)?);
        }
        Ok(options)
    }

    pub fn choices(&self, credentials: &CredentialStore) -> Result<Vec<ModelChoice>> {
        self.options()?
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
        if let Some(model) = catalog::find(&saved.provider, &saved.model) {
            ensure!(
                saved.endpoint.is_none()
                    && saved.wire.is_none()
                    && saved.api_key_env.is_none()
                    && !saved.image_input,
                "catalog model route cannot be overridden; use a custom provider identifier"
            );
            return Ok(Selection {
                provider: saved.provider.clone(),
                model: saved.model.clone(),
                endpoint: model.endpoint.into(),
                wire: match model.wire {
                    catalog::CatalogWire::ChatCompletions => HttpWire::ChatCompletions,
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
            max_output_tokens: 8192,
            context_window_tokens: None,
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
    let parent = path.parent().context("model config path has no parent")?;
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
        .mode(0o600)
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
                image_input: false,
            })
            .unwrap();
        assert_eq!(
            custom.capabilities,
            catalog::ModelCapabilities::conservative()
        );

        let _ = fs::remove_dir_all(root);
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
                .choose(
                    None,
                    None,
                    None,
                    &CredentialStore::new(root.join("credentials"))
                )
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
                    .choose(
                        None,
                        None,
                        None,
                        &CredentialStore::new(root.join("credentials"))
                    )
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
            .choose(
                None,
                None,
                None,
                &CredentialStore::new(root.join("credentials")),
            )
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
        let custom = SavedSelection {
            provider: "desktop".into(),
            model: "qwen".into(),
            endpoint: Some("http://127.0.0.1:8080/v1/chat/completions".into()),
            wire: Some(Wire::LlamaCppNoThinking),
            api_key_env: None,
            image_input: false,
        };
        store.save_default(&custom).unwrap();
        store
            .save_default(&SavedSelection {
                provider: "desktop".into(),
                model: "qwen".into(),
                endpoint: None,
                wire: None,
                api_key_env: None,
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
                image_input: false,
            })
            .unwrap();
        let credentials = CredentialStore::new(root.join("credentials"));
        let selected = store
            .choose(
                None,
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
    }
}
