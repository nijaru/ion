//! Model fetching from various providers.

use super::super::{ModelInfo, ModelPricing, Provider};
use super::types::{ApiArchitecture, ModelsResponse};
use super::ModelRegistry;
use anyhow::{Context, Result};
use serde::Deserialize;

impl ModelRegistry {
    /// Fetch models for the given provider.
    ///
    /// This is the primary entry point for model discovery. Each provider has its own
    /// fetching strategy:
    /// - OpenRouter: Direct API call
    /// - Ollama: Local server API call
    /// - Kimi: Moonshot API /v1/models endpoint
    /// - OAuth providers: Map to underlying API provider
    /// - Others: models.dev metadata fallback
    pub async fn fetch_models_for_provider(&self, provider: Provider) -> Result<Vec<ModelInfo>> {
        tracing::debug!("fetch_models_for_provider: {:?}", provider);

        match provider {
            Provider::OpenRouter => self.fetch_openrouter_models().await,
            Provider::Ollama => self.fetch_ollama_models().await,
            Provider::Kimi => self.fetch_kimi_models().await,
            // OAuth providers use same models as their underlying API
            Provider::ChatGpt => self.fetch_from_models_dev(Provider::OpenAI).await,
            Provider::Gemini => self.fetch_from_models_dev(Provider::Google).await,
            // Cloud providers: use models.dev metadata
            _ => self.fetch_from_models_dev(provider).await,
        }
    }

    /// Fetch models from Moonshot AI (Kimi) API.
    pub(crate) async fn fetch_kimi_models(&self) -> Result<Vec<ModelInfo>> {
        #[derive(Deserialize)]
        struct KimiModelsResponse {
            data: Vec<KimiModel>,
        }

        #[derive(Deserialize)]
        struct KimiModel {
            id: String,
        }

        let api_key = Provider::Kimi.api_key().unwrap_or_default();
        if api_key.is_empty() {
            anyhow::bail!("Kimi API key not set (MOONSHOT_API_KEY or KIMI_API_KEY)");
        }

        let response = self
            .client
            .get("https://api.moonshot.ai/v1/models")
            .header("Authorization", format!("Bearer {api_key}"))
            .send()
            .await
            .context("Failed to fetch models from Kimi")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Kimi API error {status}: {text}");
        }

        let data: KimiModelsResponse = response
            .json()
            .await
            .context("Failed to parse Kimi models response")?;

        let models: Vec<ModelInfo> = data
            .data
            .into_iter()
            .map(|m| {
                // Infer context window from model name
                let context_window = if m.id.contains("128k") {
                    128_000
                } else if m.id.contains("32k") {
                    32_768
                } else if m.id.contains("8k") {
                    8_192
                } else if m.id.contains("k2") {
                    256_000
                } else {
                    128_000 // Default for auto and unknown
                };

                ModelInfo {
                    id: m.id.clone(),
                    name: m.id.clone(),
                    provider: "kimi".to_string(),
                    context_window,
                    supports_tools: true,
                    supports_vision: m.id.contains("vision"),
                    supports_thinking: m.id.contains("thinking"),
                    supports_cache: false,
                    pricing: ModelPricing::default(),
                    created: 0,
                }
            })
            .collect();

        Ok(models)
    }

    /// Fetch models from Ollama local server.
    pub(crate) async fn fetch_ollama_models(&self) -> Result<Vec<ModelInfo>> {
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
    pub(crate) async fn fetch_ollama_model_info(&self, name: &str) -> ModelInfo {
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
                        info.get(&key)
                            .and_then(serde_json::Value::as_u64)
                            .map(|v| v as u32)
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
    pub(crate) async fn fetch_from_models_dev(&self, provider: Provider) -> Result<Vec<ModelInfo>> {
        let all_models = crate::provider::models_dev::fetch_models_dev()
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

    /// Fetch models from OpenRouter API.
    pub(crate) async fn fetch_openrouter_models(&self) -> Result<Vec<ModelInfo>> {
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
            .map(|m| Self::convert_api_model(m))
            .collect();

        Ok(models)
    }

    /// Convert an API model response to ModelInfo.
    fn convert_api_model(m: super::types::ApiModel) -> ModelInfo {
        let supports_cache = m.pricing.cache_read.is_some_and(|p| p > 0.0);
        let supports_vision = m
            .architecture
            .as_ref()
            .and_then(|a: &ApiArchitecture| a.modality.as_ref())
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
    }
}
