//! Two-stage model picker: Provider → Model selection.

use crate::provider::{ModelFilter, ModelInfo, ModelRegistry, ProviderPrefs};
use crate::tui::filter_input::FilterInputState;
use crate::tui::types::SelectionState;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::collections::BTreeMap;

/// Selection stage for the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerStage {
    Provider,
    Model,
}

/// Provider with aggregated stats.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub name: String,
    pub model_count: usize,
    pub min_price: f64,
    pub has_cache: bool,
}

/// State for the two-stage model picker modal.
pub struct ModelPicker {
    /// Current selection stage.
    pub stage: PickerStage,
    /// All available models (fetched from registry).
    pub all_models: Vec<ModelInfo>,
    /// Models grouped by provider.
    pub providers: Vec<ProviderEntry>,
    /// Filtered providers based on search.
    pub filtered_providers: Vec<ProviderEntry>,
    /// Models for selected provider.
    pub provider_models: Vec<ModelInfo>,
    /// Filtered models based on search.
    pub filtered_models: Vec<ModelInfo>,
    /// Filter input state.
    pub filter_input: FilterInputState,
    /// Provider list state.
    pub provider_state: SelectionState,
    /// Model list state.
    pub model_state: SelectionState,
    /// Selected provider name.
    pub selected_provider: Option<String>,
    /// Provider preferences for filtering.
    pub prefs: ProviderPrefs,
    /// Loading state.
    pub is_loading: bool,
    /// Error message if fetch failed.
    pub error: Option<String>,
    /// Current API provider name (e.g., "OpenRouter", "Ollama").
    pub api_provider_name: Option<String>,
}

impl Default for ModelPicker {
    fn default() -> Self {
        Self {
            stage: PickerStage::Provider,
            all_models: Vec::new(),
            providers: Vec::new(),
            filtered_providers: Vec::new(),
            provider_models: Vec::new(),
            filtered_models: Vec::new(),
            filter_input: FilterInputState::default(),
            provider_state: SelectionState::default(),
            model_state: SelectionState::default(),
            selected_provider: None,
            prefs: ProviderPrefs::default(),
            is_loading: false,
            error: None,
            api_provider_name: None,
        }
    }
}

impl ModelPicker {
    pub fn new(prefs: ProviderPrefs) -> Self {
        Self {
            prefs,
            ..Default::default()
        }
    }

    /// Reset picker to provider stage (for Ctrl+P: Provider → Model flow).
    pub fn reset(&mut self) {
        self.stage = PickerStage::Provider;
        self.filter_input.clear();
        self.selected_provider = None;
        self.provider_models.clear();
        self.filtered_models.clear();
        self.model_state.select(None);
        self.apply_provider_filter();
    }

    /// Start picker directly at model stage for a specific provider (for Ctrl+M: Model only).
    pub fn start_model_only(&mut self, provider: &str) {
        self.selected_provider = Some(provider.to_string());
        self.provider_models = self
            .all_models
            .iter()
            .filter(|m| m.provider == provider)
            .cloned()
            .collect();
        self.stage = PickerStage::Model;
        self.filter_input.clear();
        self.apply_model_filter();
    }

    /// Start picker showing all models (no provider filter).
    pub fn start_all_models(&mut self) {
        self.selected_provider = None;
        self.provider_models = self.all_models.clone();
        // Sorting handled by apply_model_filter()
        self.stage = PickerStage::Model;
        self.filter_input.clear();
        self.apply_model_filter();
    }

    /// Check if we have models loaded.
    pub fn has_models(&self) -> bool {
        !self.all_models.is_empty()
    }

    /// Set the API provider name (e.g., "OpenRouter", "Ollama").
    pub fn set_api_provider(&mut self, name: impl Into<String>) {
        self.api_provider_name = Some(name.into());
    }

    /// Set models from registry and build provider list.
    pub fn set_models(&mut self, models: Vec<ModelInfo>) {
        self.all_models = models;
        self.build_provider_list();
        self.apply_provider_filter();
        self.is_loading = false;
    }

    /// Set error state.
    pub fn set_error(&mut self, err: String) {
        self.error = Some(err);
        self.is_loading = false;
    }

    /// Build aggregated provider list from models.
    fn build_provider_list(&mut self) {
        let mut by_provider: BTreeMap<String, Vec<&ModelInfo>> = BTreeMap::new();

        for model in &self.all_models {
            by_provider
                .entry(model.provider.clone())
                .or_default()
                .push(model);
        }

        self.providers = by_provider
            .into_iter()
            .map(|(name, models)| {
                let min_price = models
                    .iter()
                    .map(|m| m.pricing.input)
                    .fold(f64::INFINITY, f64::min);
                let has_cache = models.iter().any(|m| m.supports_cache);

                ProviderEntry {
                    name,
                    model_count: models.len(),
                    min_price: if min_price.is_infinite() {
                        0.0
                    } else {
                        min_price
                    },
                    has_cache,
                }
            })
            .collect();

        // Sort: cache-supporting first if preferred, then by min price
        if self.prefs.prefer_cache {
            self.providers
                .sort_by(|a, b| match b.has_cache.cmp(&a.has_cache) {
                    std::cmp::Ordering::Equal => a
                        .min_price
                        .partial_cmp(&b.min_price)
                        .unwrap_or(std::cmp::Ordering::Equal),
                    other => other,
                });
        } else {
            self.providers.sort_by(|a, b| {
                a.min_price
                    .partial_cmp(&b.min_price)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }

    /// Apply filter to provider list (fuzzy match).
    fn apply_provider_filter(&mut self) {
        let matcher = SkimMatcherV2::default().ignore_case();
        let filter = self.filter_input.text();
        self.filtered_providers = self
            .providers
            .iter()
            .filter(|p| filter.is_empty() || matcher.fuzzy_match(&p.name, filter).is_some())
            .cloned()
            .collect();

        if !self.filtered_providers.is_empty() {
            self.provider_state.select(Some(0));
        } else {
            self.provider_state.select(None);
        }
    }

    /// Apply filter to model list (fuzzy match on id and name).
    fn apply_model_filter(&mut self) {
        let matcher = SkimMatcherV2::default().ignore_case();
        let filter = self.filter_input.text();
        self.filtered_models = self
            .provider_models
            .iter()
            .filter(|m| {
                filter.is_empty()
                    || matcher.fuzzy_match(&m.id, filter).is_some()
                    || matcher.fuzzy_match(&m.name, filter).is_some()
            })
            .cloned()
            .collect();

        // Sort: org first, then newest first (by created timestamp descending)
        self.filtered_models.sort_by(|a, b| {
            // Primary: org name
            a.provider.cmp(&b.provider).then_with(|| {
                // Secondary: newest first (higher created = newer)
                b.created.cmp(&a.created)
            })
        });

        if !self.filtered_models.is_empty() {
            self.model_state.select(Some(0));
        } else {
            self.model_state.select(None);
        }
    }

    pub fn apply_filter(&mut self) {
        match self.stage {
            PickerStage::Provider => self.apply_provider_filter(),
            PickerStage::Model => self.apply_model_filter(),
        }
    }

    /// Select current provider and move to model stage.
    pub fn select_provider(&mut self) {
        if let Some(idx) = self.provider_state.selected()
            && let Some(provider) = self.filtered_providers.get(idx)
        {
            self.selected_provider = Some(provider.name.clone());
            self.provider_models = self
                .all_models
                .iter()
                .filter(|m| m.provider == provider.name)
                .cloned()
                .collect();
            self.stage = PickerStage::Model;
            self.filter_input.clear();
            self.apply_model_filter();
        }
    }

    /// Go back to provider stage.
    pub fn back_to_providers(&mut self) {
        self.stage = PickerStage::Provider;
        self.filter_input.clear();
        self.selected_provider = None;
        self.apply_provider_filter();
    }

    /// Move selection up.
    pub fn move_up(&mut self, count: usize) {
        match self.stage {
            PickerStage::Provider => {
                if self.filtered_providers.is_empty() {
                    return;
                }
                let i = self.provider_state.selected().unwrap_or(0);
                let new_i = i.saturating_sub(count);
                self.provider_state.select(Some(new_i));
            }
            PickerStage::Model => {
                if self.filtered_models.is_empty() {
                    return;
                }
                let i = self.model_state.selected().unwrap_or(0);
                let new_i = i.saturating_sub(count);
                self.model_state.select(Some(new_i));
            }
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self, count: usize) {
        match self.stage {
            PickerStage::Provider => {
                if self.filtered_providers.is_empty() {
                    return;
                }
                let len = self.filtered_providers.len();
                let i = self.provider_state.selected().unwrap_or(0);
                let new_i = (i + count).min(len - 1);
                self.provider_state.select(Some(new_i));
            }
            PickerStage::Model => {
                if self.filtered_models.is_empty() {
                    return;
                }
                let len = self.filtered_models.len();
                let i = self.model_state.selected().unwrap_or(0);
                let new_i = (i + count).min(len - 1);
                self.model_state.select(Some(new_i));
            }
        }
    }

    /// Jump to top.
    pub fn jump_to_top(&mut self) {
        match self.stage {
            PickerStage::Provider => {
                if !self.filtered_providers.is_empty() {
                    self.provider_state.select(Some(0));
                }
            }
            PickerStage::Model => {
                if !self.filtered_models.is_empty() {
                    self.model_state.select(Some(0));
                }
            }
        }
    }

    /// Jump to bottom.
    pub fn jump_to_bottom(&mut self) {
        match self.stage {
            PickerStage::Provider => {
                if !self.filtered_providers.is_empty() {
                    self.provider_state
                        .select(Some(self.filtered_providers.len() - 1));
                }
            }
            PickerStage::Model => {
                if !self.filtered_models.is_empty() {
                    self.model_state
                        .select(Some(self.filtered_models.len() - 1));
                }
            }
        }
    }

    /// Get currently selected model (only valid in Model stage).
    pub fn selected_model(&self) -> Option<&ModelInfo> {
        if self.stage != PickerStage::Model {
            return None;
        }
        self.model_state
            .selected()
            .and_then(|i| self.filtered_models.get(i))
    }
}

use crate::provider::Provider;

/// Fetch models from registry for the given provider.
pub async fn fetch_models_for_picker(
    registry: &ModelRegistry,
    provider: Provider,
    prefs: &ProviderPrefs,
) -> Result<Vec<ModelInfo>, anyhow::Error> {
    let models = registry.fetch_models_for_provider(provider).await?;
    let filter = ModelFilter::default();
    Ok(registry.list_models_from_vec(models, &filter, prefs))
}
