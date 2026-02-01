//! Types for model registry and API responses.

use super::super::ModelInfo;
use serde::Deserialize;
use std::time::Instant;

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
pub(crate) struct ModelCache {
    pub(crate) models: Vec<ModelInfo>,
    pub(crate) fetched_at: Option<Instant>,
}

/// OpenRouter API response structures.
#[derive(Debug, Deserialize)]
pub(crate) struct ModelsResponse {
    pub(crate) data: Vec<ApiModel>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiModel {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) context_length: u32,
    #[serde(default)]
    pub(crate) created: u64,
    pub(crate) pricing: ApiPricing,
    #[serde(default)]
    pub(crate) architecture: Option<ApiArchitecture>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiPricing {
    #[serde(default, deserialize_with = "parse_price")]
    pub(crate) prompt: f64,
    #[serde(default, deserialize_with = "parse_price")]
    pub(crate) completion: f64,
    #[serde(default, deserialize_with = "parse_optional_price")]
    pub(crate) cache_read: Option<f64>,
    #[serde(default, deserialize_with = "parse_optional_price")]
    pub(crate) cache_write: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiArchitecture {
    pub(crate) modality: Option<String>,
    #[serde(default)]
    pub(crate) instruct_type: Option<String>,
}

/// Parse price string to f64 (API returns strings like "0.00025").
pub(crate) fn parse_price<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    s.parse().unwrap_or(0.0).pipe(Ok)
}

pub(crate) fn parse_optional_price<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Deserialize::deserialize(deserializer)?;
    Ok(opt.and_then(|s| s.parse().ok()))
}

/// Helper trait for pipe syntax.
pub(crate) trait Pipe: Sized {
    fn pipe<F, R>(self, f: F) -> R
    where
        F: FnOnce(Self) -> R,
    {
        f(self)
    }
}

impl<T> Pipe for T {}
