//! Model filtering and sorting.

use super::super::{prefs::SortStrategy, ModelInfo, ProviderPrefs};
use super::types::ModelFilter;
use super::ModelRegistry;

impl ModelRegistry {
    /// List models matching filter criteria from a provided list.
    pub fn list_models_from_vec(
        &self,
        models: Vec<ModelInfo>,
        filter: &ModelFilter,
        prefs: &ProviderPrefs,
    ) -> Vec<ModelInfo> {
        let mut filtered: Vec<ModelInfo> = models
            .into_iter()
            .filter(|m| Self::model_matches_filter(m, filter, prefs))
            .collect();

        Self::sort_models(&mut filtered, filter, prefs);
        filtered
    }

    /// List models matching filter criteria.
    pub fn list_models(&self, filter: &ModelFilter, prefs: &ProviderPrefs) -> Vec<ModelInfo> {
        let cache = self
            .cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut models: Vec<ModelInfo> = cache
            .models
            .iter()
            .filter(|m| Self::model_matches_filter(m, filter, prefs))
            .cloned()
            .collect();

        Self::sort_models(&mut models, filter, prefs);
        models
    }

    /// Sort models according to preferences.
    pub(crate) fn sort_models(
        models: &mut [ModelInfo],
        filter: &ModelFilter,
        prefs: &ProviderPrefs,
    ) {
        models.sort_by(|a, b| {
            // Preferred providers first
            if let Some(ref prefer) = prefs.prefer {
                let a_preferred = prefer.iter().any(|p| p.eq_ignore_ascii_case(&a.provider));
                let b_preferred = prefer.iter().any(|p| p.eq_ignore_ascii_case(&b.provider));
                if a_preferred != b_preferred {
                    return b_preferred.cmp(&a_preferred);
                }
            }

            // Cache-supporting models first if preferred
            if (filter.prefer_cache || prefs.prefer_cache) && a.supports_cache != b.supports_cache {
                return b.supports_cache.cmp(&a.supports_cache);
            }

            // Sort by strategy
            match prefs.sort.unwrap_or_default() {
                SortStrategy::Alphabetical => {
                    // Sort by org, then by newest first (created descending)
                    match a.provider.cmp(&b.provider) {
                        std::cmp::Ordering::Equal => b.created.cmp(&a.created),
                        other => other,
                    }
                }
                SortStrategy::Price => match a.pricing.input.partial_cmp(&b.pricing.input) {
                    Some(ordering) => ordering,
                    None => std::cmp::Ordering::Equal,
                },
                SortStrategy::Throughput => {
                    // Higher throughput is better, use context as proxy
                    b.context_window.cmp(&a.context_window)
                }
                SortStrategy::Latency => {
                    // Smaller models generally have lower latency
                    a.context_window.cmp(&b.context_window)
                }
                SortStrategy::Newest => match b.created.cmp(&a.created) {
                    std::cmp::Ordering::Equal => match a.provider.cmp(&b.provider) {
                        std::cmp::Ordering::Equal => a.name.cmp(&b.name),
                        other => other,
                    },
                    other => other,
                },
            }
        });
    }

    /// Check if a model passes the filter criteria.
    pub(crate) fn model_matches_filter(
        model: &ModelInfo,
        filter: &ModelFilter,
        prefs: &ProviderPrefs,
    ) -> bool {
        // Min context check
        if let Some(min) = filter.min_context
            && model.context_window < min
        {
            return false;
        }

        // Tool support check
        if filter.require_tools && !model.supports_tools {
            return false;
        }

        // Vision support check
        if filter.require_vision && !model.supports_vision {
            return false;
        }

        // Max input price check
        if let Some(max) = filter.max_input_price
            && model.pricing.input > max
        {
            return false;
        }

        // ID prefix check
        if let Some(ref prefix) = filter.id_prefix
            && !model.id.to_lowercase().contains(&prefix.to_lowercase())
        {
            return false;
        }

        // Provider ignore list
        if let Some(ref ignore) = prefs.ignore
            && ignore
                .iter()
                .any(|p| p.eq_ignore_ascii_case(&model.provider))
        {
            return false;
        }

        // Provider only list
        if let Some(ref only) = prefs.only
            && !only.iter().any(|p| p.eq_ignore_ascii_case(&model.provider))
        {
            return false;
        }

        true
    }
}
