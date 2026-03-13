//! Provider selection and model fetching.

use crate::agent::{Agent, AgentEvent};
use crate::provider::{Client, LlmApi, ModelInfo, ModelRegistry, Provider};
use crate::tui::App;
use crate::tui::model_picker;
use crate::tui::types::{Mode, SelectorPage};
use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::debug;

impl App {
    /// Set the active API provider and re-create the agent.
    pub(in crate::tui) fn set_provider(&mut self, api_provider: Provider) -> Result<()> {
        // For OAuth providers, use from_provider_sync (no token refresh in sync context).
        // Token refresh happens at startup via from_provider() which is async.
        // For regular providers, get API key from config.
        let (provider, api_key): (Arc<dyn LlmApi>, String) = if api_provider.is_oauth() {
            let client = Client::from_provider_sync(api_provider)
                .context("Failed to create OAuth client - run 'ion login' first")?;
            (Arc::new(client), String::new())
        } else {
            let api_key = self
                .config
                .api_key_for(api_provider.id())
                .unwrap_or_default();
            let client = Client::new(api_provider, api_key.clone())
                .context("Failed to create LLM client")?;
            (Arc::new(client), api_key)
        };

        self.api_provider = api_provider;

        // Save provider to config
        self.config.provider = Some(api_provider.id().to_string());
        if let Err(e) = self.config.save() {
            tracing::warn!("Failed to save config: {}", e);
        }

        // Re-create agent with new provider but same orchestrator
        let mut agent = Agent::new(provider, self.orchestrator.clone());
        if let Some(ref prompt) = self.config.system_prompt {
            agent = agent.with_system_prompt(prompt.clone());
        }
        self.agent = Arc::new(agent);

        // Update model registry with new key/base if it's OpenRouter
        if api_provider == Provider::OpenRouter {
            self.model_registry = Arc::new(ModelRegistry::new(
                api_key,
                self.config.model_cache_ttl_secs,
            ));
        }

        // Set API provider name on model picker
        self.model_picker.set_api_provider(api_provider.name());

        // Clear old models when switching providers
        self.model_picker.set_models(vec![]);
        self.model_picker.is_loading = true;
        self.setup_fetch_started = false;
        Ok(())
    }

    /// Open model selector (Ctrl+M or during setup).
    pub(in crate::tui) fn open_model_selector(&mut self) {
        self.mode = Mode::Selector;
        self.selector_page = SelectorPage::Model;
        self.model_picker.error = None;

        if self.model_picker.has_models() {
            // Already loaded this session — show immediately.
            self.model_picker.start_all_models();
        } else {
            self.setup_fetch_started = true;
            // Load from disk cache so the list is populated immediately while
            // the background fetch runs to refresh it.
            if let Some(cached) = self.load_model_cache(self.api_provider) {
                self.model_picker.set_models(cached);
                self.model_picker.start_all_models();
            }
            // set_models() clears is_loading; set it after any cache load so
            // the background fetch always runs and the UI shows loading state.
            self.model_picker.is_loading = true;
            self.fetch_models();
        }
    }

    /// Path to the on-disk model cache for a given provider.
    fn model_cache_path(&self, provider: Provider) -> std::path::PathBuf {
        self.config
            .data_dir
            .join(format!("models_{}.json", provider.id()))
    }

    /// Load cached models from disk. Returns `None` if no cache exists or it
    /// cannot be parsed.
    pub(in crate::tui) fn load_model_cache(&self, provider: Provider) -> Option<Vec<ModelInfo>> {
        let data = std::fs::read(self.model_cache_path(provider)).ok()?;
        serde_json::from_slice(&data).ok()
    }

    /// Persist the model list to disk so the next session can load it immediately.
    pub(in crate::tui) fn save_model_cache(&self, provider: Provider, models: &[ModelInfo]) {
        match serde_json::to_vec(models) {
            Ok(data) => {
                if let Err(e) = std::fs::write(self.model_cache_path(provider), data) {
                    tracing::warn!("Failed to write model cache: {e}");
                }
            }
            Err(e) => tracing::warn!("Failed to serialize model cache: {e}"),
        }
    }

    /// Open API provider selector (Ctrl+P).
    pub(in crate::tui) fn open_provider_selector(&mut self) {
        self.mode = Mode::Selector;
        self.selector_page = SelectorPage::Provider;
        self.provider_picker.refresh();
        self.provider_picker.select_provider(self.api_provider);
    }

    /// Open session selector (/resume).
    pub fn open_session_selector(&mut self) {
        self.mode = Mode::Selector;
        self.selector_page = SelectorPage::Session;
        self.session_picker.load_sessions(&self.store, 50);
    }

    /// Preview models for a provider without committing to it.
    /// Used when user selects a different provider - shows models before confirming.
    pub(in crate::tui) fn preview_provider_models(&mut self, provider: Provider) {
        // Set up model picker to show this provider's models
        self.model_picker.set_api_provider(provider.name());
        self.model_picker.set_models(vec![]);
        self.model_picker.is_loading = true;
        self.mode = Mode::Selector;
        self.selector_page = SelectorPage::Model;
        self.model_picker.error = None;

        // Fetch models for the preview provider
        self.fetch_models_for_provider(provider);
    }

    /// Fetch models asynchronously for a specific provider.
    pub(in crate::tui) fn fetch_models_for_provider(&self, provider: Provider) {
        debug!("Starting model fetch for {:?}", provider);
        let registry = self.model_registry.clone();
        let prefs = self.config.provider_prefs.clone();
        let agent_tx = self.agent_tx.clone();

        tokio::spawn(async move {
            debug!("Model fetch task started for {:?}", provider);
            match model_picker::fetch_models_for_picker(&registry, provider, &prefs).await {
                Ok(models) => {
                    debug!("Fetched {} models", models.len());
                    let _ = agent_tx.send(AgentEvent::ModelsFetched(models)).await;
                }
                Err(e) => {
                    debug!("Model fetch error: {}", e);
                    let _ = agent_tx
                        .send(AgentEvent::ModelFetchError(e.to_string()))
                        .await;
                }
            }
        });
    }

    /// Fetch models asynchronously.
    pub(in crate::tui) fn fetch_models(&self) {
        debug!("Starting model fetch");
        let registry = self.model_registry.clone();
        let provider = self.api_provider;
        let prefs = self.config.provider_prefs.clone();
        let agent_tx = self.agent_tx.clone();

        tokio::spawn(async move {
            debug!("Model fetch task started for {:?}", provider);
            match model_picker::fetch_models_for_picker(&registry, provider, &prefs).await {
                Ok(models) => {
                    debug!("Fetched {} models", models.len());
                    let _ = agent_tx.send(AgentEvent::ModelsFetched(models)).await;
                }
                Err(e) => {
                    debug!("Model fetch error: {}", e);
                    let _ = agent_tx
                        .send(AgentEvent::ModelFetchError(e.to_string()))
                        .await;
                }
            }
        });
    }
}
