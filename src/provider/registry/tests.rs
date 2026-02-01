//! Tests for model registry.

#[cfg(test)]
mod tests {
    use super::super::super::{prefs::SortStrategy, ModelInfo, ModelPricing, Provider, ProviderPrefs};
    use super::super::types::ModelFilter;
    use super::super::ModelRegistry;
    use std::time::Instant;

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
            sort: Some(SortStrategy::Price),
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
