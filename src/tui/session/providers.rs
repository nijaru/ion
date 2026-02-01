//! Provider selection and model fetching.

use crate::agent::{Agent, AgentEvent};
use crate::provider::{Client, LlmApi, ModelRegistry, Provider};
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
            // Show all models directly (user can type to filter)
            self.model_picker.start_all_models();
        } else {
            // Need to fetch models first - update() will configure picker when they arrive
            self.model_picker.is_loading = true;
            self.setup_fetch_started = true;
            self.fetch_models();
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
