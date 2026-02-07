//! Tests for model registry.

use super::super::{prefs::SortStrategy, ModelInfo, ModelPricing, Provider, ProviderPrefs};
use super::types::ModelFilter;
use super::ModelRegistry;
use std::time::Instant;

fn make_test_model(id: &str, provider: &str, price: f64, has_cache: bool) -> ModelInfo {
    make_test_model_dated(id, provider, price, has_cache, 0)
}

fn make_test_model_dated(
    id: &str,
    provider: &str,
    price: f64,
    has_cache: bool,
    created: u64,
) -> ModelInfo {
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
        created,
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
async fn test_fetch_local_models() {
    // Skip if local server isn't running
    let base_url = std::env::var("ION_LOCAL_URL")
        .unwrap_or_else(|_| "http://localhost:8080/v1".to_string());
    let client = reqwest::Client::new();
    if client
        .get(format!("{base_url}/models"))
        .send()
        .await
        .is_err()
    {
        eprintln!("Skipping test: local LLM server not running");
        return;
    }

    let registry = ModelRegistry::new("".into(), 3600);
    let models = registry.fetch_models_for_provider(Provider::Local).await;
    assert!(models.is_ok(), "fetch_models_for_provider should succeed");
    let models = models.unwrap();
    // Local server may have 0 or more models depending on /v1/models support
    for model in &models {
        assert!(!model.id.is_empty());
        assert_eq!(model.provider, "local");
    }
}

#[test]
fn test_select_summarization_model_picks_newest_cheap() {
    let models = vec![
        make_test_model_dated("old-cheap", "a", 0.10, false, 1_700_000_000),
        make_test_model_dated("new-cheap", "a", 0.12, false, 1_750_000_000),
        make_test_model_dated("expensive", "a", 15.0, false, 1_760_000_000),
    ];
    let picked = ModelRegistry::select_summarization_model(&models).unwrap();
    assert_eq!(picked.id, "new-cheap");
}

#[test]
fn test_select_summarization_model_no_pricing() {
    let models = vec![ModelInfo {
        id: "local-model".to_string(),
        name: "local-model".to_string(),
        provider: "local".to_string(),
        context_window: 32_000,
        supports_tools: true,
        supports_vision: false,
        supports_thinking: false,
        supports_cache: false,
        pricing: ModelPricing::default(), // zero pricing
        created: 0,
    }];
    assert!(ModelRegistry::select_summarization_model(&models).is_none());
}

#[test]
fn test_select_summarization_model_skips_small_context() {
    let models = vec![
        make_test_model_dated("tiny-ctx", "a", 0.05, false, 1_750_000_000),
    ];
    // Override context window to be too small
    let mut models = models;
    models[0].context_window = 4_000;
    assert!(ModelRegistry::select_summarization_model(&models).is_none());
}
