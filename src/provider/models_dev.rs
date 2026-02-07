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
    /// Release date (YYYY-MM-DD format from models.dev).
    #[serde(default)]
    pub release_date: Option<String>,
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

/// Parse a "YYYY-MM-DD" date string into a unix timestamp (seconds since epoch).
fn parse_release_date(date: &str) -> u64 {
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() != 3 {
        return 0;
    }
    let (y, m, d) = match (
        parts[0].parse::<i64>(),
        parts[1].parse::<u32>(),
        parts[2].parse::<u32>(),
    ) {
        (Ok(y), Ok(m), Ok(d)) => (y, m, d),
        _ => return 0,
    };
    // Reject values that would cause overflow in the calendar math below
    if y < 1970 || m == 0 || m > 12 || d == 0 || d > 31 {
        return 0;
    }
    // Days from epoch (1970-01-01) using a simple calendar calculation.
    // Accurate enough for sorting — exact second precision isn't needed.
    let days = {
        let m_adj = if m > 2 { m - 3 } else { m + 9 };
        let y_adj = if m <= 2 { y - 1 } else { y };
        let era = y_adj / 400;
        let yoe = (y_adj - era * 400) as u64;
        let doy = (153 * m_adj as u64 + 2) / 5 + d as u64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        (era * 146097) as u64 + doe - 719_468
    };
    days * 86400
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

            let created = m
                .release_date
                .as_deref()
                .map_or(0, parse_release_date);

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
                created,
            });
        }
    }

    Ok(all_models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_release_date() {
        // Known date: 2025-06-20 -> 1750377600
        let ts = parse_release_date("2025-06-20");
        assert!(ts > 0);
        // Should be in the right ballpark (2025 is ~1.74B seconds from epoch)
        assert!(ts > 1_700_000_000, "timestamp {ts} too small");
        assert!(ts < 1_800_000_000, "timestamp {ts} too large");
    }

    #[test]
    fn test_parse_release_date_ordering() {
        let older = parse_release_date("2024-01-15");
        let newer = parse_release_date("2025-11-25");
        assert!(newer > older);
    }

    #[test]
    fn test_parse_release_date_invalid() {
        assert_eq!(parse_release_date(""), 0);
        assert_eq!(parse_release_date("not-a-date"), 0);
        assert_eq!(parse_release_date("2025"), 0);
        // Malformed dates that would cause overflow without guards
        assert_eq!(parse_release_date("2025-03-00"), 0); // day 0
        assert_eq!(parse_release_date("2025-00-15"), 0); // month 0
        assert_eq!(parse_release_date("2025-13-15"), 0); // month 13
        assert_eq!(parse_release_date("0001-01-15"), 0); // pre-epoch year
    }

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

    #[tokio::test]
    async fn test_models_have_release_dates() {
        let models = fetch_models_dev().await.unwrap();
        let with_dates = models.iter().filter(|m| m.created > 0).count();
        // Most models should have dates
        assert!(
            with_dates > models.len() / 2,
            "Only {with_dates}/{} models have release dates",
            models.len()
        );
    }
}
