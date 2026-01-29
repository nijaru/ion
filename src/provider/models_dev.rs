use super::{ModelInfo, ModelPricing};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct ModelsDevProvider {
    #[allow(dead_code)]
    pub name: String,
    pub models: HashMap<String, ModelsDevEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ModelsDevEntry {
    pub name: String,
    #[serde(default)]
    pub tool_call: Option<bool>,
    #[serde(default)]
    pub modalities: ModelsDevModalities,
    pub cost: Option<ModelsDevCost>,
    pub limit: Option<ModelsDevLimit>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ModelsDevModalities {
    #[serde(default)]
    pub input: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ModelsDevCost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
}

#[derive(Debug, Deserialize)]
pub struct ModelsDevLimit {
    #[serde(default)]
    pub context: u32,
}

pub async fn fetch_models_dev() -> Result<Vec<ModelInfo>> {
    let client = crate::provider::create_http_client();
    let response = client
        .get("https://models.dev/api.json")
        .send()
        .await
        .context("Failed to fetch models from models.dev")?;

    if !response.status().is_success() {
        anyhow::bail!("Models.dev API error: {}", response.status());
    }

    let data: HashMap<String, ModelsDevProvider> = response
        .json()
        .await
        .context("Failed to parse models.dev JSON")?;

    let mut all_models = Vec::new();

    for (provider_id, provider_data) in data {
        for (model_id, m) in provider_data.models {
            let supports_tools = m.tool_call.unwrap_or(false);
            let supports_vision = m.modalities.input.iter().any(|m| m == "image");

            // models.dev uses per-million pricing directly, matching our internal ModelPricing unit.
            let pricing = if let Some(cost) = m.cost {
                ModelPricing {
                    input: cost.input,
                    output: cost.output,
                    cache_read: None,
                    cache_write: None,
                }
            } else {
                ModelPricing::default()
            };

            all_models.push(ModelInfo {
                // Use native model ID (what the API expects), not prefixed
                id: model_id.clone(),
                name: m.name,
                provider: provider_id.clone(),
                context_window: m.limit.map_or(0, |l| l.context),
                supports_tools,
                supports_vision,
                supports_thinking: false,
                supports_cache: false,
                pricing,
                created: 0,
            });
        }
    }

    Ok(all_models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fetch_models_dev() {
        let models = fetch_models_dev().await.unwrap();
        assert!(!models.is_empty());

        // Check for known models (IDs are native model names, not prefixed)
        let has_claude = models
            .iter()
            .any(|m| m.provider == "anthropic" && m.id.contains("claude"));
        let has_gpt = models
            .iter()
            .any(|m| m.provider == "openai" && m.id.contains("gpt"));

        assert!(has_claude || has_gpt, "Should contain some major models");
    }
}
