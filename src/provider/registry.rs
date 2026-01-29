//! Model registry with caching and filtering.
//!
//! Fetches model metadata from various backends and caches locally.

use super::{ModelInfo, ModelPricing, Provider, ProviderPrefs};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::RwLock;
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
#[derive(Default)]
struct ModelCache {
    models: Vec<ModelInfo>,
    fetched_at: Option<Instant>,
}

/// Registry for fetching and filtering models.
pub struct ModelRegistry {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    cache: RwLock<ModelCache>,
    ttl: Duration,
}

/// `OpenRouter` API response structures.
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ApiModel>,
}

#[derive(Debug, Deserialize)]
struct ApiModel {
    id: String,
    name: String,
    context_length: u32,
    #[serde(default)]
    created: u64,
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
    #[must_use] 
    pub fn new(api_key: String, ttl_secs: u64) -> Self {
        Self {
            client: crate::provider::create_http_client(),
            api_key,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            cache: RwLock::new(ModelCache::default()),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// Check if cache is valid.
    fn cache_valid(&self) -> bool {
        let cache = self.cache.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache
            .fetched_at
            .is_some_and(|t| t.elapsed() < self.ttl)
    }

    /// Fetch models for the given provider.
    ///
    /// This is the primary entry point for model discovery. Each provider has its own
    /// fetching strategy:
    /// - `OpenRouter`: Direct API call
    /// - Ollama: Local server API call
    /// - Kimi: Static model list (Moonshot API doesn't have model listing)
    /// - Others: models.dev metadata fallback
    pub async fn fetch_models_for_provider(&self, provider: Provider) -> Result<Vec<ModelInfo>> {
        tracing::debug!("fetch_models_for_provider: {:?}", provider);

        match provider {
            Provider::OpenRouter => self.fetch_openrouter_models().await,
            Provider::Ollama => self.fetch_ollama_models().await,
            Provider::Kimi => Ok(Self::kimi_models()),
            // Cloud providers: use models.dev metadata
            _ => self.fetch_from_models_dev(provider).await,
        }
    }

    /// Static list of Kimi models (Moonshot API doesn't provide model listing endpoint).
    fn kimi_models() -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "moonshot-v1-auto".to_string(),
                name: "Kimi Auto".to_string(),
                provider: "kimi".to_string(),
                context_window: 128_000,
                supports_tools: true,
                supports_vision: false,
                supports_thinking: false,
                supports_cache: false,
                pricing: ModelPricing {
                    input: 0.12,  // Per million tokens
                    output: 0.12,
                    cache_read: None,
                    cache_write: None,
                },
                created: 0,
            },
            ModelInfo {
                id: "moonshot-v1-8k".to_string(),
                name: "Kimi 8K".to_string(),
                provider: "kimi".to_string(),
                context_window: 8192,
                supports_tools: true,
                supports_vision: false,
                supports_thinking: false,
                supports_cache: false,
                pricing: ModelPricing {
                    input: 0.12,
                    output: 0.12,
                    cache_read: None,
                    cache_write: None,
                },
                created: 0,
            },
            ModelInfo {
                id: "moonshot-v1-32k".to_string(),
                name: "Kimi 32K".to_string(),
                provider: "kimi".to_string(),
                context_window: 32768,
                supports_tools: true,
                supports_vision: false,
                supports_thinking: false,
                supports_cache: false,
                pricing: ModelPricing {
                    input: 0.24,
                    output: 0.24,
                    cache_read: None,
                    cache_write: None,
                },
                created: 0,
            },
            ModelInfo {
                id: "moonshot-v1-128k".to_string(),
                name: "Kimi 128K".to_string(),
                provider: "kimi".to_string(),
                context_window: 128_000,
                supports_tools: true,
                supports_vision: false,
                supports_thinking: false,
                supports_cache: false,
                pricing: ModelPricing {
                    input: 0.60,
                    output: 0.60,
                    cache_read: None,
                    cache_write: None,
                },
                created: 0,
            },
            ModelInfo {
                id: "kimi-k2-0528-preview".to_string(),
                name: "Kimi K2".to_string(),
                provider: "kimi".to_string(),
                context_window: 256_000,
                supports_tools: true,
                supports_vision: false,
                supports_thinking: false,
                supports_cache: false,
                pricing: ModelPricing {
                    input: 2.0,  // Estimated pricing
                    output: 8.0,
                    cache_read: None,
                    cache_write: None,
                },
                created: 0,
            },
        ]
    }

    /// Fetch models from Ollama local server.
    async fn fetch_ollama_models(&self) -> Result<Vec<ModelInfo>> {
        #[derive(Deserialize)]
        struct OllamaTagsResponse {
            models: Vec<OllamaModel>,
        }

        #[derive(Deserialize)]
        struct OllamaModel {
            name: String,
        }

        let base_url = "http://localhost:11434";

        let response = self
            .client
            .get(format!("{base_url}/api/tags"))
            .send()
            .await
            .context("Failed to connect to Ollama - is it running?")?;

        if !response.status().is_success() {
            anyhow::bail!("Ollama returned status {}", response.status());
        }

        let data: OllamaTagsResponse = response
            .json()
            .await
            .context("Failed to parse Ollama response")?;

        // Fetch details for each model in parallel
        let mut models = Vec::new();
        for m in data.models {
            let info = self.fetch_ollama_model_info(&m.name).await;
            models.push(info);
        }

        Ok(models)
    }

    /// Fetch detailed info for a single Ollama model.
    async fn fetch_ollama_model_info(&self, name: &str) -> ModelInfo {
        let base_url = "http://localhost:11434";

        // Try to get model details from /api/show
        // Context length is stored at {architecture}.context_length, not general.context_length
        let context_window = match self
            .client
            .post(format!("{base_url}/api/show"))
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                #[derive(Deserialize)]
                struct OllamaShowResponse {
                    #[serde(default)]
                    model_info: Option<std::collections::HashMap<String, serde_json::Value>>,
                }

                response
                    .json::<OllamaShowResponse>()
                    .await
                    .ok()
                    .and_then(|r| r.model_info)
                    .and_then(|info| {
                        // Get architecture name (e.g., "qwen3next", "mistral3", "llama")
                        let arch = info.get("general.architecture").and_then(|v| v.as_str())?;
                        // Context length is at {architecture}.context_length
                        let key = format!("{arch}.context_length");
                        #[allow(clippy::cast_possible_truncation)] // Context lengths fit in u32
                        info.get(&key).and_then(serde_json::Value::as_u64).map(|v| v as u32)
                    })
                    .unwrap_or(32768) // Conservative default for modern models
            }
            _ => 32768, // Conservative default for modern models
        };

        ModelInfo {
            id: name.to_string(),
            name: name.to_string(),
            provider: "ollama".to_string(),
            context_window,
            supports_tools: true,
            supports_vision: false,
            supports_thinking: false,
            supports_cache: false,
            pricing: ModelPricing::default(),
            created: 0,
        }
    }

    /// Fetch models from models.dev, filtered by provider.
    async fn fetch_from_models_dev(&self, provider: Provider) -> Result<Vec<ModelInfo>> {
        let all_models = super::models_dev::fetch_models_dev()
            .await
            .unwrap_or_default();

        // Filter by provider name matching the provider
        let provider_name = provider.id();
        let filtered: Vec<ModelInfo> = all_models
            .into_iter()
            .filter(|m| m.provider.to_lowercase() == provider_name)
            .collect();

        Ok(filtered)
    }

    /// Fetch models from `OpenRouter` API and Models.dev.
    pub async fn fetch_models(&self) -> Result<()> {
        let mut all_models = Vec::new();

        // 1. Try fetching from OpenRouter if API key is present
        if !self.api_key.is_empty()
            && let Ok(or_models) = self.fetch_openrouter_models().await
        {
            all_models.extend(or_models);
        }

        // 2. Fetch from Models.dev (universal source for direct provider access)
        if let Ok(md_models) = super::models_dev::fetch_models_dev().await {
            // Add models not already present (OpenRouter uses "provider/model" IDs,
            // models.dev uses native model names - they won't conflict)
            for m in md_models {
                if !all_models.iter().any(|existing| existing.id == m.id) {
                    all_models.push(m);
                }
            }
        }

        let mut cache = self.cache.write().unwrap_or_else(std::sync::PoisonError::into_inner);
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
            anyhow::bail!("OpenRouter error {status}: {text}");
        }

        let data: ModelsResponse = response
            .json()
            .await
            .context("Failed to parse OpenRouter models response")?;

        let models: Vec<ModelInfo> = data
            .data
            .into_iter()
            .map(|m| {
                let supports_cache = m.pricing.cache_read.is_some_and(|p| p > 0.0);
                let supports_vision = m
                    .architecture
                    .as_ref()
                    .and_then(|a| a.modality.as_ref())
                    .is_some_and(|modality| modality.contains("image"));
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
                    created: m.created,
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
        Ok(self
            .cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .models
            .clone())
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
            .filter(|m| Self::model_matches_filter(m, filter, prefs))
            .collect();

        Self::sort_models(&mut filtered, filter, prefs);
        filtered
    }

    /// List models matching filter criteria.
    pub fn list_models(&self, filter: &ModelFilter, prefs: &ProviderPrefs) -> Vec<ModelInfo> {
        let cache = self.cache.read().unwrap_or_else(std::sync::PoisonError::into_inner);
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
    fn sort_models(models: &mut [ModelInfo], filter: &ModelFilter, prefs: &ProviderPrefs) {
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
                super::prefs::SortStrategy::Alphabetical => {
                    // Sort by org, then by newest first (created descending)
                    match a.provider.cmp(&b.provider) {
                        std::cmp::Ordering::Equal => b.created.cmp(&a.created),
                        other => other,
                    }
                }
                super::prefs::SortStrategy::Price => {
                    match a.pricing.input.partial_cmp(&b.pricing.input) {
                        Some(ordering) => ordering,
                        None => std::cmp::Ordering::Equal,
                    }
                }
                super::prefs::SortStrategy::Throughput => {
                    // Higher throughput is better, use context as proxy
                    b.context_window.cmp(&a.context_window)
                }
                super::prefs::SortStrategy::Latency => {
                    // Smaller models generally have lower latency
                    a.context_window.cmp(&b.context_window)
                }
                super::prefs::SortStrategy::Newest => {
                    match b.created.cmp(&a.created) {
                        std::cmp::Ordering::Equal => match a.provider.cmp(&b.provider) {
                            std::cmp::Ordering::Equal => a.name.cmp(&b.name),
                            other => other,
                        },
                        other => other,
                    }
                }
            }
        });
    }

    /// Check if a model passes the filter criteria.
    fn model_matches_filter(
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
            && !only
                .iter()
                .any(|p| p.eq_ignore_ascii_case(&model.provider))
        {
            return false;
        }

        true
    }

    /// Get a specific model by ID.
    pub fn get_model(&self, id: &str) -> Option<ModelInfo> {
        let cache = self.cache.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.models.iter().find(|m| m.id == id).cloned()
    }

    /// Get cached model count.
    pub fn model_count(&self) -> usize {
        self.cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .models
            .len()
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

    #[tokio::test]
    async fn test_fetch_ollama_models() {
        // Skip if Ollama isn't running
        let client = reqwest::Client::new();
        if client
            .get("http://localhost:11434/api/tags")
            .send()
            .await
            .is_err()
        {
            eprintln!("Skipping test: Ollama not running");
            return;
        }

        let registry = ModelRegistry::new("".into(), 3600);
        let models = registry.fetch_models_for_provider(Provider::Ollama).await;
        assert!(models.is_ok(), "fetch_models_for_provider should succeed");
        let models = models.unwrap();
        assert!(!models.is_empty(), "Ollama should have at least one model");
        for model in &models {
            assert!(!model.id.is_empty());
            assert_eq!(model.provider, "ollama");
        }
    }
}
