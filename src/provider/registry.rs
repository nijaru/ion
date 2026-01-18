//! Model registry with caching and filtering.
//!
//! Fetches model metadata from OpenRouter API and caches locally.

use super::{ModelInfo, ModelPricing, Provider, ProviderPrefs};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Filter criteria for model queries.
#[derive(Debug, Clone, Default)]
pub struct ModelFilter {
    pub min_context: Option<u32>,
    pub require_tools: bool,
    pub require_vision: bool,
    pub prefer_cache: bool,
    pub max_input_price: Option<f64>,
    pub id_prefix: Option<String>,
}

/// Cached model list with TTL.
struct ModelCache {
    models: Vec<ModelInfo>,
    fetched_at: Option<Instant>,
}

impl Default for ModelCache {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            fetched_at: None,
        }
    }
}

/// Registry for fetching and filtering models.
pub struct ModelRegistry {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    cache: RwLock<ModelCache>,
    ttl: Duration,
}

/// OpenRouter API response structures.
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ApiModel>,
}

#[derive(Debug, Deserialize)]
struct ApiModel {
    id: String,
    name: String,
    context_length: u32,
    pricing: ApiPricing,
    #[serde(default)]
    architecture: Option<ApiArchitecture>,
}

#[derive(Debug, Deserialize)]
struct ApiPricing {
    #[serde(default, deserialize_with = "parse_price")]
    prompt: f64,
    #[serde(default, deserialize_with = "parse_price")]
    completion: f64,
    #[serde(default, deserialize_with = "parse_optional_price")]
    cache_read: Option<f64>,
    #[serde(default, deserialize_with = "parse_optional_price")]
    cache_write: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ApiArchitecture {
    modality: Option<String>,
    #[serde(default)]
    instruct_type: Option<String>,
}

/// Parse price string to f64 (API returns strings like "0.00025").
fn parse_price<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    s.parse().unwrap_or(0.0).pipe(Ok)
}

fn parse_optional_price<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Deserialize::deserialize(deserializer)?;
    Ok(opt.and_then(|s| s.parse().ok()))
}

/// Helper trait for pipe syntax.
trait Pipe: Sized {
    fn pipe<F, R>(self, f: F) -> R
    where
        F: FnOnce(Self) -> R,
    {
        f(self)
    }
}

impl<T> Pipe for T {}

impl ModelRegistry {
    pub fn new(api_key: String, ttl_secs: u64) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            cache: RwLock::new(ModelCache::default()),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// Check if cache is valid.
    fn cache_valid(&self) -> bool {
        let cache = self.cache.read().unwrap();
        cache
            .fetched_at
            .map(|t| t.elapsed() < self.ttl)
            .unwrap_or(false)
    }

    /// Fetch models using a hybrid approach:
    /// 1. Query the active provider for availability.
    /// 2. Hydrate metadata (pricing, context) from models.dev.
    pub async fn fetch_hybrid(&self, provider: Arc<dyn Provider>) -> Result<Vec<ModelInfo>> {
        tracing::debug!("fetch_hybrid: calling provider.list_models()");
        let mut available_models = provider
            .list_models()
            .await
            .map_err(|e| anyhow::anyhow!("Provider list error: {}", e))?;
        tracing::debug!("fetch_hybrid: provider returned {} models", available_models.len());

        // Fetch metadata from models.dev (cached)
        let metadata_list = if !self.cache_valid() {
            super::models_dev::fetch_models_dev()
                .await
                .unwrap_or_default()
        } else {
            self.cache.read().unwrap().models.clone()
        };

        // Hydrate available models with metadata
        for model in &mut available_models {
            // Find match in models.dev
            // Match criteria: ID match (normalized) or name match
            let found = metadata_list.iter().find(|m| {
                m.id == model.id
                    || m.id.replace(':', "/") == model.id
                    || m.id.split(':').last() == Some(&model.id)
            });

            if let Some(meta) = found {
                model.name = meta.name.clone();
                model.context_window = meta.context_window;
                model.supports_tools = meta.supports_tools;
                model.supports_vision = meta.supports_vision;
                model.pricing = meta.pricing.clone();
                model.supports_cache = meta.supports_cache;
            } else {
                // Fallback for new models: use generic defaults if context is 0
                if model.context_window == 0 {
                    model.context_window = 128_000;
                }
            }
        }

        // Cache the metadata list for future lookups
        if !metadata_list.is_empty() {
            let mut cache = self.cache.write().unwrap();
            cache.models = metadata_list;
            cache.fetched_at = Some(Instant::now());
        }

        Ok(available_models)
    }

    /// Fetch models from OpenRouter API and Models.dev.
    pub async fn fetch_models(&self) -> Result<()> {
        let mut all_models = Vec::new();

        // 1. Try fetching from OpenRouter if API key is present
        if !self.api_key.is_empty() {
            if let Ok(or_models) = self.fetch_openrouter_models().await {
                all_models.extend(or_models);
            }
        }

        // 2. Fetch from Models.dev (universal source)
        if let Ok(md_models) = super::models_dev::fetch_models_dev().await {
            // Avoid duplicates by checking IDs
            for m in md_models {
                // If we already have this model from OpenRouter, keep OpenRouter's (likely more up to date for OR users)
                // OpenRouter IDs are "provider/model", Models.dev are "provider:model"
                // We should probably normalize or check name match too
                let normalized_id = m.id.replace(':', "/");
                if !all_models
                    .iter()
                    .any(|existing| existing.id == normalized_id || existing.id == m.id)
                {
                    all_models.push(m);
                }
            }
        }

        let mut cache = self.cache.write().unwrap();
        cache.models = all_models;
        cache.fetched_at = Some(Instant::now());

        Ok(())
    }

    async fn fetch_openrouter_models(&self) -> Result<Vec<ModelInfo>> {
        let response = self
            .client
            .get(format!("{}/models", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .context("Failed to fetch models from OpenRouter")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenRouter error {}: {}", status, text);
        }

        let data: ModelsResponse = response
            .json()
            .await
            .context("Failed to parse OpenRouter models response")?;

        let models: Vec<ModelInfo> = data
            .data
            .into_iter()
            .map(|m| {
                let supports_cache = m.pricing.cache_read.map(|p| p > 0.0).unwrap_or(false);
                let supports_vision = m
                    .architecture
                    .as_ref()
                    .and_then(|a| a.modality.as_ref())
                    .map(|modality| modality.contains("image"))
                    .unwrap_or(false);
                let provider = m.id.split('/').next().unwrap_or("unknown").to_string();
                let supports_tools = m
                    .architecture
                    .as_ref()
                    .and_then(|a| a.instruct_type.as_ref())
                    .is_some();

                ModelInfo {
                    id: m.id,
                    name: m.name,
                    provider,
                    context_window: m.context_length,
                    supports_tools,
                    supports_vision,
                    supports_thinking: false,
                    supports_cache,
                    pricing: ModelPricing {
                        input: m.pricing.prompt * 1_000_000.0,
                        output: m.pricing.completion * 1_000_000.0,
                        cache_read: m.pricing.cache_read.map(|p| p * 1_000_000.0),
                        cache_write: m.pricing.cache_write.map(|p| p * 1_000_000.0),
                    },
                    created: 0,
                }
            })
            .collect();

        Ok(models)
    }

    /// Get models, fetching if cache is stale.
    pub async fn get_models(&self) -> Result<Vec<ModelInfo>> {
        if !self.cache_valid() {
            self.fetch_models().await?;
        }
        Ok(self.cache.read().unwrap().models.clone())
    }

    /// List models matching filter criteria from a provided list.
    pub fn list_models_from_vec(
        &self,
        models: Vec<ModelInfo>,
        filter: &ModelFilter,
        prefs: &ProviderPrefs,
    ) -> Vec<ModelInfo> {
        let mut filtered: Vec<ModelInfo> = models
            .into_iter()
            .filter(|m| {
                // Min context check
                if let Some(min) = filter.min_context {
                    if m.context_window < min {
                        return false;
                    }
                }

                // Tool support check
                if filter.require_tools && !m.supports_tools {
                    return false;
                }

                // Vision support check
                if filter.require_vision && !m.supports_vision {
                    return false;
                }

                // Max input price check
                if let Some(max) = filter.max_input_price {
                    if m.pricing.input > max {
                        return false;
                    }
                }

                // ID prefix check
                if let Some(ref prefix) = filter.id_prefix {
                    if !m.id.to_lowercase().contains(&prefix.to_lowercase()) {
                        return false;
                    }
                }

                true
            })
            .collect();

        // Sort by preferences
        self.sort_models(&mut filtered, filter, prefs);

        filtered
    }

    /// List models matching filter criteria.
    pub fn list_models(&self, filter: &ModelFilter, prefs: &ProviderPrefs) -> Vec<ModelInfo> {
        let cache = self.cache.read().unwrap();
        let mut models: Vec<ModelInfo> = cache
            .models
            .iter()
            .filter(|m| {
                // Min context check
                if let Some(min) = filter.min_context {
                    if m.context_window < min {
                        return false;
                    }
                }

                // Tool support check
                if filter.require_tools && !m.supports_tools {
                    return false;
                }

                // Vision support check
                if filter.require_vision && !m.supports_vision {
                    return false;
                }

                // Max input price check
                if let Some(max) = filter.max_input_price {
                    if m.pricing.input > max {
                        return false;
                    }
                }

                // ID prefix check
                if let Some(ref prefix) = filter.id_prefix {
                    if !m.id.to_lowercase().contains(&prefix.to_lowercase()) {
                        return false;
                    }
                }

                // Provider ignore list
                if let Some(ref ignore) = prefs.ignore {
                    if ignore.iter().any(|p| p.eq_ignore_ascii_case(&m.provider)) {
                        return false;
                    }
                }

                // Provider only list
                if let Some(ref only) = prefs.only {
                    if !only.iter().any(|p| p.eq_ignore_ascii_case(&m.provider)) {
                        return false;
                    }
                }

                true
            })
            .cloned()
            .collect();

        // Sort by preferences
        self.sort_models(&mut models, filter, prefs);

        models
    }

    /// Sort models according to preferences.
    fn sort_models(&self, models: &mut [ModelInfo], filter: &ModelFilter, prefs: &ProviderPrefs) {
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
            if filter.prefer_cache || prefs.prefer_cache {
                if a.supports_cache != b.supports_cache {
                    return b.supports_cache.cmp(&a.supports_cache);
                }
            }

            // Sort by strategy
            match prefs.sort.unwrap_or_default() {
                super::prefs::SortStrategy::Price => {
                    a.pricing.input.partial_cmp(&b.pricing.input).unwrap()
                }
                super::prefs::SortStrategy::Throughput => {
                    // Higher throughput is better, use context as proxy
                    b.context_window.cmp(&a.context_window)
                }
                super::prefs::SortStrategy::Latency => {
                    // Smaller models generally have lower latency
                    a.context_window.cmp(&b.context_window)
                }
            }
        });
    }

    /// Get a specific model by ID.
    pub fn get_model(&self, id: &str) -> Option<ModelInfo> {
        let cache = self.cache.read().unwrap();
        cache.models.iter().find(|m| m.id == id).cloned()
    }

    /// Get cached model count.
    pub fn model_count(&self) -> usize {
        self.cache.read().unwrap().models.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_model(id: &str, provider: &str, price: f64, has_cache: bool) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            name: id.to_string(),
            provider: provider.to_string(),
            context_window: 128_000,
            supports_tools: true,
            supports_vision: false,
            supports_thinking: false,
            supports_cache: has_cache,
            pricing: ModelPricing {
                input: price,
                output: price * 3.0,
                cache_read: if has_cache { Some(price * 0.1) } else { None },
                cache_write: if has_cache { Some(price * 1.25) } else { None },
            },
            created: 0,
        }
    }

    #[test]
    fn test_filter_by_provider_ignore() {
        let registry = ModelRegistry::new("test".into(), 3600);

        // Manually populate cache for testing
        {
            let mut cache = registry.cache.write().unwrap();
            cache.models = vec![
                make_test_model("anthropic/claude-sonnet-4", "anthropic", 3.0, true),
                make_test_model("openai/gpt-4o", "openai", 2.5, true),
                make_test_model("deepseek/deepseek-chat", "deepseek", 0.14, false),
            ];
            cache.fetched_at = Some(Instant::now());
        }

        let filter = ModelFilter::default();
        let prefs = ProviderPrefs {
            ignore: Some(vec!["openai".to_string()]),
            ..Default::default()
        };

        let models = registry.list_models(&filter, &prefs);
        assert_eq!(models.len(), 2);
        assert!(models.iter().all(|m| m.provider != "openai"));
    }

    #[test]
    fn test_filter_prefer_cache() {
        let registry = ModelRegistry::new("test".into(), 3600);

        {
            let mut cache = registry.cache.write().unwrap();
            cache.models = vec![
                make_test_model("model-a", "provider-a", 1.0, false),
                make_test_model("model-b", "provider-b", 1.0, true),
                make_test_model("model-c", "provider-c", 1.0, false),
            ];
            cache.fetched_at = Some(Instant::now());
        }

        let filter = ModelFilter {
            prefer_cache: true,
            ..Default::default()
        };
        let prefs = ProviderPrefs::default();

        let models = registry.list_models(&filter, &prefs);
        // Cache-supporting model should be first
        assert!(models[0].supports_cache);
    }

    #[test]
    fn test_filter_by_id_prefix() {
        let registry = ModelRegistry::new("test".into(), 3600);

        {
            let mut cache = registry.cache.write().unwrap();
            cache.models = vec![
                make_test_model("anthropic/claude-sonnet-4", "anthropic", 3.0, true),
                make_test_model("anthropic/claude-opus-4", "anthropic", 15.0, true),
                make_test_model("openai/gpt-4o", "openai", 2.5, true),
            ];
            cache.fetched_at = Some(Instant::now());
        }

        let filter = ModelFilter {
            id_prefix: Some("claude".to_string()),
            ..Default::default()
        };
        let prefs = ProviderPrefs::default();

        let models = registry.list_models(&filter, &prefs);
        assert_eq!(models.len(), 2);
        assert!(models.iter().all(|m| m.id.contains("claude")));
    }

    #[test]
    fn test_sort_by_price() {
        let registry = ModelRegistry::new("test".into(), 3600);

        {
            let mut cache = registry.cache.write().unwrap();
            cache.models = vec![
                make_test_model("expensive", "a", 10.0, false),
                make_test_model("cheap", "b", 0.1, false),
                make_test_model("medium", "c", 2.0, false),
            ];
            cache.fetched_at = Some(Instant::now());
        }

        let filter = ModelFilter::default();
        let prefs = ProviderPrefs {
            sort: Some(super::super::prefs::SortStrategy::Price),
            ..Default::default()
        };

        let models = registry.list_models(&filter, &prefs);
        assert_eq!(models[0].id, "cheap");
        assert_eq!(models[1].id, "medium");
        assert_eq!(models[2].id, "expensive");
    }
}
