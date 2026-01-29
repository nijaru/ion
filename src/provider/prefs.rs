//! Provider preferences for `OpenRouter` model routing.
//!
//! Supports quantization filtering, provider selection, and sorting.
//! Based on orcx patterns for three-layer config with merge precedence.

use serde::{Deserialize, Serialize};

/// Known `OpenRouter` providers for validation.
pub const KNOWN_PROVIDERS: &[&str] = &[
    "Anthropic",
    "Azure",
    "AWS Bedrock",
    "Google",
    "Google AI Studio",
    "Groq",
    "Lepton",
    "Mistral",
    "Novita",
    "OpenAI",
    "Together",
    "DeepInfra",
    "Fireworks",
    "SambaNova",
    "Lambda",
    "Lynn",
    "Mancer",
    "Mancer 2",
    "Hyperbolic",
    "OctoAI",
    "Cloudflare",
    "Crusoe",
    "DeepSeek",
    "Avian",
    "SF Compute",
    "Nineteen.ai",
    "Inference.net",
    "Featherless",
    "Kluster.ai",
    "Inflection",
    "xAI",
    "Chutes",
    "NexusFlow",
    "SiliconFlow",
    "Modal",
    "AnyScale",
];

/// Quantization mappings for `min_bits` resolution.
fn quantizations_for_min_bits(min_bits: u8) -> Vec<String> {
    match min_bits {
        32 => vec!["fp32".into()],
        16 => vec!["fp32".into(), "fp16".into(), "bf16".into()],
        8 => vec!["fp32".into(), "fp16".into(), "bf16".into(), "int8".into()],
        4 => vec![
            "fp32".into(),
            "fp16".into(),
            "bf16".into(),
            "int8".into(),
            "int4".into(),
        ],
        _ => vec![],
    }
}

/// Sort strategies for model selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortStrategy {
    /// Alphabetical by org, then by model name
    #[default]
    Alphabetical,
    /// Cheapest input price first
    Price,
    /// Highest context/throughput first
    Throughput,
    /// Lowest latency (smaller models) first
    Latency,
    /// Newest models first (uses created timestamp when available)
    Newest,
}

/// Provider preferences for filtering and routing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderPrefs {
    // Quantization filtering
    /// Explicit quantization formats to allow.
    pub quantizations: Option<Vec<String>>,
    /// Quantization formats to exclude.
    pub exclude_quants: Option<Vec<String>>,
    /// Minimum bits (4, 8, 16, 32) - resolves to quantizations.
    pub min_bits: Option<u8>,

    // Provider selection
    /// Providers to ignore (blacklist).
    pub ignore: Option<Vec<String>>,
    /// Only use these providers (whitelist).
    pub only: Option<Vec<String>>,
    /// Preferred providers (sorted first).
    pub prefer: Option<Vec<String>>,
    /// Explicit provider order.
    pub order: Option<Vec<String>>,
    /// Allow fallback to non-preferred providers.
    #[serde(default = "default_allow_fallbacks")]
    pub allow_fallbacks: bool,

    // Sorting
    /// Sort strategy for model selection.
    pub sort: Option<SortStrategy>,

    // Caching preference
    /// Prefer models with prompt caching support.
    #[serde(default)]
    pub prefer_cache: bool,
}

fn default_allow_fallbacks() -> bool {
    true
}

impl ProviderPrefs {
    /// Create empty preferences (no filtering).
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Merge self with other, self takes precedence for scalars.
    /// Lists are unioned.
    #[must_use]
    pub fn merge_with(&self, other: &Self) -> Self {
        Self {
            quantizations: merge_option_vec(&self.quantizations, &other.quantizations),
            exclude_quants: merge_option_vec(&self.exclude_quants, &other.exclude_quants),
            min_bits: self.min_bits.or(other.min_bits),
            ignore: merge_option_vec(&self.ignore, &other.ignore),
            only: merge_option_vec(&self.only, &other.only),
            prefer: merge_option_vec(&self.prefer, &other.prefer),
            order: self.order.clone().or_else(|| other.order.clone()),
            allow_fallbacks: self.allow_fallbacks && other.allow_fallbacks,
            sort: self.sort.or(other.sort),
            prefer_cache: self.prefer_cache || other.prefer_cache,
        }
    }

    /// Resolve `min_bits` to a quantization list.
    /// Returns explicit quantizations if set, otherwise resolves from `min_bits`.
    pub fn resolve_quantizations(&self) -> Option<Vec<String>> {
        if self.quantizations.is_some() {
            return self.quantizations.clone();
        }
        self.min_bits.map(quantizations_for_min_bits)
    }

    /// Validate provider names against known list.
    /// Returns typo suggestions for unknown providers.
    #[must_use]
    pub fn validate_providers(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        let check_providers = |providers: &Option<Vec<String>>, field: &str| {
            let mut field_warnings = Vec::new();
            if let Some(list) = providers {
                for provider in list {
                    if !KNOWN_PROVIDERS
                        .iter()
                        .any(|k| k.eq_ignore_ascii_case(provider))
                    {
                        if let Some(suggestion) = find_similar_provider(provider) {
                            field_warnings.push(format!(
                                "{field}: unknown provider '{provider}', did you mean '{suggestion}'?"
                            ));
                        } else {
                            field_warnings.push(format!("{field}: unknown provider '{provider}'"));
                        }
                    }
                }
            }
            field_warnings
        };

        warnings.extend(check_providers(&self.ignore, "ignore"));
        warnings.extend(check_providers(&self.only, "only"));
        warnings.extend(check_providers(&self.prefer, "prefer"));
        warnings.extend(check_providers(&self.order, "order"));

        warnings
    }

    /// Convert to `OpenRouter` provider routing parameters.
    #[must_use]
    pub fn to_routing_params(&self) -> Option<serde_json::Value> {
        let mut provider = serde_json::Map::new();
        let mut has_content = false;

        if let Some(ref quantizations) = self.resolve_quantizations() {
            provider.insert(
                "quantizations".to_string(),
                serde_json::json!(quantizations),
            );
            has_content = true;
        }

        if let Some(ref ignore) = self.ignore {
            provider.insert("ignore".to_string(), serde_json::json!(ignore));
            has_content = true;
        }

        if let Some(ref only) = self.only {
            provider.insert("only".to_string(), serde_json::json!(only));
            has_content = true;
        }

        if let Some(ref order) = self.order {
            provider.insert("order".to_string(), serde_json::json!(order));
            has_content = true;
        }

        if !self.allow_fallbacks {
            provider.insert("allow_fallbacks".to_string(), serde_json::json!(false));
            has_content = true;
        }

        if let Some(sort) = self.sort {
            // Alphabetical and Newest are local-only, not sent to OpenRouter
            let sort_str = match sort {
                SortStrategy::Alphabetical | SortStrategy::Newest => None,
                SortStrategy::Price => Some("price"),
                SortStrategy::Throughput => Some("throughput"),
                SortStrategy::Latency => Some("latency"),
            };
            if let Some(s) = sort_str {
                provider.insert("sort".to_string(), serde_json::json!(s));
                has_content = true;
            }
        }

        if has_content {
            Some(serde_json::Value::Object(provider))
        } else {
            None
        }
    }
}

/// Merge two optional vectors, unioning their contents.
fn merge_option_vec(a: &Option<Vec<String>>, b: &Option<Vec<String>>) -> Option<Vec<String>> {
    match (a, b) {
        (Some(a_vec), Some(b_vec)) => {
            let mut merged = a_vec.clone();
            for item in b_vec {
                if !merged.contains(item) {
                    merged.push(item.clone());
                }
            }
            Some(merged)
        }
        (Some(a_vec), None) => Some(a_vec.clone()),
        (None, Some(b_vec)) => Some(b_vec.clone()),
        (None, None) => None,
    }
}

/// Find a similar provider name for typo suggestions.
fn find_similar_provider(input: &str) -> Option<&'static str> {
    let input_lower = input.to_lowercase();
    KNOWN_PROVIDERS
        .iter()
        .filter(|&known| {
            let known_lower = known.to_lowercase();
            // Check for substring match or Levenshtein-ish similarity
            known_lower.contains(&input_lower)
                || input_lower.contains(&known_lower)
                || levenshtein_distance(&input_lower, &known_lower) <= 2
        })
        .min_by_key(|&known| levenshtein_distance(&input.to_lowercase(), &known.to_lowercase()))
        .copied()
}

/// Simple Levenshtein distance for typo detection.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut prev_row: Vec<usize> = (0..=n).collect();
    let mut curr_row: Vec<usize> = vec![0; n + 1];

    for i in 1..=m {
        curr_row[0] = i;
        for j in 1..=n {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            curr_row[j] = (prev_row[j] + 1)
                .min(curr_row[j - 1] + 1)
                .min(prev_row[j - 1] + cost);
        }
        std::mem::swap(&mut prev_row, &mut curr_row);
    }

    prev_row[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_min_bits_resolution() {
        let prefs = ProviderPrefs {
            min_bits: Some(8),
            ..Default::default()
        };
        let quants = prefs.resolve_quantizations().unwrap();
        assert!(quants.contains(&"fp32".to_string()));
        assert!(quants.contains(&"int8".to_string()));
        assert!(!quants.contains(&"int4".to_string()));
    }

    #[test]
    fn test_explicit_quantizations_override_min_bits() {
        let prefs = ProviderPrefs {
            min_bits: Some(8),
            quantizations: Some(vec!["fp16".to_string()]),
            ..Default::default()
        };
        let quants = prefs.resolve_quantizations().unwrap();
        assert_eq!(quants, vec!["fp16".to_string()]);
    }

    #[test]
    fn test_merge_with_precedence() {
        let a = ProviderPrefs {
            min_bits: Some(8),
            ignore: Some(vec!["ProviderA".to_string()]),
            sort: Some(SortStrategy::Price),
            ..Default::default()
        };
        let b = ProviderPrefs {
            min_bits: Some(16),
            ignore: Some(vec!["ProviderB".to_string()]),
            sort: Some(SortStrategy::Latency),
            prefer_cache: true,
            ..Default::default()
        };

        let merged = a.merge_with(&b);

        // Scalar: a takes precedence
        assert_eq!(merged.min_bits, Some(8));
        assert_eq!(merged.sort, Some(SortStrategy::Price));

        // Lists: union
        let ignore = merged.ignore.unwrap();
        assert!(ignore.contains(&"ProviderA".to_string()));
        assert!(ignore.contains(&"ProviderB".to_string()));

        // Booleans: OR for prefer_cache
        assert!(merged.prefer_cache);
    }

    #[test]
    fn test_validate_providers_with_typo() {
        let prefs = ProviderPrefs {
            ignore: Some(vec!["Antropic".to_string()]), // typo
            ..Default::default()
        };
        let warnings = prefs.validate_providers();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Anthropic")); // suggestion
    }

    #[test]
    fn test_to_routing_params() {
        let prefs = ProviderPrefs {
            ignore: Some(vec!["DeepInfra".to_string()]),
            sort: Some(SortStrategy::Price),
            allow_fallbacks: false,
            ..Default::default()
        };
        let params = prefs.to_routing_params().unwrap();
        assert_eq!(params["ignore"], serde_json::json!(["DeepInfra"]));
        assert_eq!(params["sort"], serde_json::json!("price"));
        assert_eq!(params["allow_fallbacks"], serde_json::json!(false));
    }

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
        assert_eq!(levenshtein_distance("", "abc"), 3);
        assert_eq!(levenshtein_distance("abc", "abc"), 0);
    }
}
