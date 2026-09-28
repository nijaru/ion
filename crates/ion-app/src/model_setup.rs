//! Host-owned model routes and the default choice. Sessions store only model identity.

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use ion_ai::ModelRef;
use ion_core::HttpWire;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    auth::{CredentialStatus, CredentialStore},
    catalog,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Wire {
    ChatCompletions,
    LlamaCppNoThinking,
    AnthropicMessages,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedSelection {
    pub provider: String,
    pub model: String,
    pub endpoint: Option<String>,
    pub wire: Option<Wire>,
    pub api_key_env: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Routes {
    custom: Vec<SavedSelection>,
}

#[derive(Clone)]
pub struct Selection {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub wire: HttpWire,
    pub api_key_env: String,
    pub max_output_tokens: u32,
    pub requires_key: bool,
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
            && credentials.status(&self.provider, &self.api_key_env)? == CredentialStatus::Missing
        {
            bail!(
                "no {} credential; set {} or run `ion login {}`",
                self.provider,
                self.api_key_env,
                self.provider
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
        let effective =
            if saved.endpoint.is_none() && catalog::find(&saved.provider, &saved.model).is_none() {
                ensure!(
                    saved.wire.is_none() && saved.api_key_env.is_none(),
                    "custom route overrides require --endpoint and --wire"
                );
                let existing =
                    self.routes()?.custom.into_iter().find(|route| {
                        route.provider == saved.provider && route.model == saved.model
                    });
                match existing {
                    Some(route) => route,
                    None => self
                        .default()?
                        .filter(|route| {
                            route.provider == saved.provider
                                && route.model == saved.model
                                && route.endpoint.is_some()
                        })
                        .context("custom model has no configured route")?,
                }
            } else {
                saved.clone()
            };
        let selected = self.resolve_saved(&effective)?;
        if effective.endpoint.is_some() {
            let mut routes = self.routes()?;
            routes.custom.retain(|route| {
                route.provider != effective.provider || route.model != effective.model
            });
            routes.custom.push(effective.clone());
            write_json(&self.root.join("routes.json"), &routes)?;
        }
        write_json(&self.root.join("selection.json"), &effective)?;
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
        if let Some(saved) = self.default()? {
            return self.resolve_saved(&saved);
        }
        for entry in catalog::models() {
            if credentials.status(entry.provider, entry.api_key_env)? != CredentialStatus::Missing {
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
        if catalog::find(&model.provider, &model.model).is_some() {
            return self.resolve_saved(&SavedSelection {
                provider: model.provider.clone(),
                model: model.model.clone(),
                endpoint: None,
                wire: None,
                api_key_env: None,
            });
        }
        let routes = self.routes()?;
        if let Some(route) = routes
            .custom
            .iter()
            .find(|route| route.provider == model.provider && route.model == model.model)
        {
            return self.resolve_saved(route);
        }
        // Existing installations may have one custom route only in selection.json.
        if let Some(saved) = self.default()?
            && saved.provider == model.provider
            && saved.model == model.model
            && saved.endpoint.is_some()
        {
            return self.resolve_saved(&saved);
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
        for route in self.routes()?.custom {
            options.push(self.resolve_saved(&route)?);
        }
        if let Some(saved) = self.default()?
            && saved.endpoint.is_some()
            && !options
                .iter()
                .any(|option| option.provider == saved.provider && option.model == saved.model)
        {
            options.push(self.resolve_saved(&saved)?);
        }
        Ok(options)
    }

    pub fn choices(&self, credentials: &CredentialStore) -> Result<Vec<ModelChoice>> {
        self.options()?
            .into_iter()
            .map(|selected| {
                let label = catalog::find(&selected.provider, &selected.model)
                    .map_or("Custom endpoint", |entry| entry.label);
                let credential = credentials.status(&selected.provider, &selected.api_key_env)?;
                Ok(ModelChoice {
                    selected,
                    label,
                    credential,
                })
            })
            .collect()
    }

    fn default(&self) -> Result<Option<SavedSelection>> {
        read_json(&self.root.join("selection.json"))
    }

    fn routes(&self) -> Result<Routes> {
        Ok(read_json(&self.root.join("routes.json"))?.unwrap_or_default())
    }

    fn resolve_saved(&self, saved: &SavedSelection) -> Result<Selection> {
        if let Some(model) = catalog::find(&saved.provider, &saved.model) {
            ensure!(
                saved.endpoint.is_none() && saved.wire.is_none() && saved.api_key_env.is_none(),
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
                    catalog::CatalogWire::OpenRouterNoReasoning => HttpWire::OpenRouterNoReasoning,
                    catalog::CatalogWire::AnthropicMessages => HttpWire::AnthropicMessages,
                },
                api_key_env: model.api_key_env.into(),
                max_output_tokens: model.max_output_tokens,
                requires_key: true,
            });
        }
        let endpoint = saved
            .endpoint
            .as_deref()
            .context("unknown model; supply --endpoint and --wire to configure a custom route")?;
        let wire = saved.wire.context("custom route requires --wire")?;
        let url = reqwest::Url::parse(endpoint)?;
        let host = url.host_str().context("custom endpoint has no host")?;
        let local = matches!(host, "127.0.0.1" | "::1" | "[::1]");
        ensure!(
            url.scheme() == "https" || (url.scheme() == "http" && local),
            "custom endpoint requires HTTPS or literal loopback HTTP"
        );
        ensure!(
            url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
            "custom endpoint must not contain credentials or a fragment"
        );
        ensure!(!saved.model.is_empty(), "model ID is empty");
        Ok(Selection {
            provider: saved.provider.clone(),
            model: saved.model.clone(),
            endpoint: endpoint.into(),
            wire: match wire {
                Wire::ChatCompletions => HttpWire::ChatCompletions,
                Wire::LlamaCppNoThinking => HttpWire::LlamaCppNoThinking,
                Wire::AnthropicMessages => HttpWire::AnthropicMessages,
            },
            api_key_env: saved
                .api_key_env
                .clone()
                .unwrap_or_else(|| "ION_CUSTOM_API_KEY".into()),
            max_output_tokens: 8192,
            requires_key: !local,
        })
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes)
                .with_context(|| format!("invalid {}", path.display()))?,
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
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
        File::open(parent)?.sync_all()?;
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
        };
        store.save_default(&custom).unwrap();
        store
            .save_default(&SavedSelection {
                provider: "desktop".into(),
                model: "qwen".into(),
                endpoint: None,
                wire: None,
                api_key_env: None,
            })
            .unwrap();
        store
            .save_default(&SavedSelection {
                provider: "openrouter".into(),
                model: "deepseek/deepseek-v4.1-flash".into(),
                endpoint: None,
                wire: None,
                api_key_env: None,
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
