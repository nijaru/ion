//! Model registry with caching and filtering.
//!
//! Fetches model metadata from various backends and caches locally.

mod fetch;
mod filter;
#[cfg(test)]
mod tests;
mod types;

use super::ModelInfo;
use anyhow::Result;
use std::sync::RwLock;
use std::time::{Duration, Instant};

pub use types::ModelFilter;
use types::ModelCache;

/// Registry for fetching and filtering models.
pub struct ModelRegistry {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    cache: RwLock<ModelCache>,
    ttl: Duration,
}

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
        let cache = self
            .cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.fetched_at.is_some_and(|t| t.elapsed() < self.ttl)
    }

    /// Fetch models from `OpenRouter` API and `Models.dev`.
    pub async fn fetch_models(&self) -> Result<()> {
        let mut all_models = Vec::new();

        // 1. Try fetching from OpenRouter if API key is present
        if !self.api_key.is_empty()
            && let Ok(or_models) = self.fetch_openrouter_models().await
        {
            all_models.extend(or_models);
        }

        // 2. Fetch from Models.dev (universal source for direct provider access)
        if let Ok(md_models) = crate::provider::models_dev::fetch_models_dev().await {
            // Add models not already present (OpenRouter uses "provider/model" IDs,
            // models.dev uses native model names - they won't conflict)
            for m in md_models {
                if !all_models.iter().any(|existing| existing.id == m.id) {
                    all_models.push(m);
                }
            }
        }

        let mut cache = self
            .cache
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.models = all_models;
        cache.fetched_at = Some(Instant::now());

        Ok(())
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

    /// Get a specific model by ID.
    pub fn get_model(&self, id: &str) -> Option<ModelInfo> {
        let cache = self
            .cache
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
