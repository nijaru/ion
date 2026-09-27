//! Curated starting points for the model wire APIs Ion currently implements.
//!
//! Checked 2026-09-26 against the linked provider model pages. A context window
//! includes input and output tokens; it is not an independent input allowance.
//! Catalog membership says the wire route exists, not that an account has access
//! to the model or that a live request has been qualified.
//! Claude 5 models emit thinking by default; Ion's current Messages boundary
//! does not preserve thinking blocks for tool replay, so they are absent here.
//! Opus/Sonnet 4.6 remain available with thinking disabled but are legacy.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogWire {
    ChatCompletions,
    DeepSeekChat,
    MiMoChat,
    OpenRouterNoReasoning,
    AnthropicMessages,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatalogModel {
    pub provider: &'static str,
    pub id: &'static str,
    pub label: &'static str,
    pub wire: CatalogWire,
    pub endpoint: &'static str,
    pub api_key_env: &'static str,
    /// Published total context window, including generated tokens.
    pub context_window: u32,
    /// Published maximum generated tokens for one request on this API.
    pub max_output_tokens: u32,
    pub source_url: &'static str,
    pub checked_on: &'static str,
}

const CHECKED_ON: &str = "2026-09-26";
const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";
const ANTHROPIC_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEEPSEEK_ENDPOINT: &str = "https://api.deepseek.com/chat/completions";
const MIMO_ENDPOINT: &str = "https://api.xiaomimimo.com/v1/chat/completions";

const MODELS: &[CatalogModel] = &[
    CatalogModel {
        provider: "deepseek",
        id: "deepseek-flash",
        label: "DeepSeek V4.1 Flash",
        wire: CatalogWire::DeepSeekChat,
        endpoint: DEEPSEEK_ENDPOINT,
        api_key_env: "DEEPSEEK_API_KEY",
        context_window: 1_048_576,
        max_output_tokens: 384_000,
        source_url: "https://api-docs.deepseek.com/quick_start/pricing/",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "xiaomi",
        id: "mimo-v2.6-flash",
        label: "MiMo V2.6 Flash",
        wire: CatalogWire::MiMoChat,
        endpoint: MIMO_ENDPOINT,
        api_key_env: "XIAOMI_API_KEY",
        context_window: 1_000_000,
        max_output_tokens: 131_072,
        source_url: "https://mimo.mi.com/models/en-US/mimo-v2.6-flash",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "openrouter",
        id: "deepseek/deepseek-v4.1-flash",
        label: "DeepSeek V4.1 Flash via OpenRouter",
        wire: CatalogWire::OpenRouterNoReasoning,
        endpoint: OPENROUTER_ENDPOINT,
        api_key_env: "OPENROUTER_API_KEY",
        context_window: 1_048_576,
        max_output_tokens: 128_000,
        source_url: "https://openrouter.ai/deepseek/deepseek-v4.1-flash/providers",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "openai",
        id: "gpt-5.4-mini",
        label: "GPT-5.4 Mini",
        wire: CatalogWire::ChatCompletions,
        endpoint: OPENAI_ENDPOINT,
        api_key_env: "OPENAI_API_KEY",
        context_window: 400_000,
        max_output_tokens: 128_000,
        source_url: "https://developers.openai.com/api/docs/models/gpt-5.4-mini",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "openai",
        id: "gpt-5.4",
        label: "GPT-5.4",
        wire: CatalogWire::ChatCompletions,
        endpoint: OPENAI_ENDPOINT,
        api_key_env: "OPENAI_API_KEY",
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        source_url: "https://developers.openai.com/api/docs/models/gpt-5.4",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "openrouter",
        id: "openai/gpt-5.4",
        label: "GPT-5.4 via OpenRouter",
        wire: CatalogWire::ChatCompletions,
        endpoint: OPENROUTER_ENDPOINT,
        api_key_env: "OPENROUTER_API_KEY",
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        source_url: "https://openrouter.ai/openai/gpt-5.4",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "anthropic",
        id: "claude-opus-4-6",
        label: "Claude Opus 4.6 (legacy)",
        wire: CatalogWire::AnthropicMessages,
        endpoint: ANTHROPIC_ENDPOINT,
        api_key_env: "ANTHROPIC_API_KEY",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        source_url: "https://platform.claude.com/docs/en/models/opus-4-6/overview",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "anthropic",
        id: "claude-sonnet-4-6",
        label: "Claude Sonnet 4.6 (legacy)",
        wire: CatalogWire::AnthropicMessages,
        endpoint: ANTHROPIC_ENDPOINT,
        api_key_env: "ANTHROPIC_API_KEY",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        source_url: "https://platform.claude.com/docs/en/models/sonnet-4-6/overview",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "anthropic",
        id: "claude-haiku-4-5-20251001",
        label: "Claude Haiku 4.5",
        wire: CatalogWire::AnthropicMessages,
        endpoint: ANTHROPIC_ENDPOINT,
        api_key_env: "ANTHROPIC_API_KEY",
        context_window: 200_000,
        max_output_tokens: 64_000,
        source_url: "https://platform.claude.com/docs/en/models/overview",
        checked_on: CHECKED_ON,
    },
];

pub fn models() -> &'static [CatalogModel] {
    MODELS
}

/// IDs are exact within a provider. Do not silently resolve an unknown ID to
/// another model, because that would change the selected billing and behavior.
pub fn find(provider: &str, id: &str) -> Option<&'static CatalogModel> {
    MODELS
        .iter()
        .find(|model| model.provider == provider && model.id == id)
}

/// Search display names and exact provider/model identifiers for a picker.
pub fn search(query: &str) -> impl Iterator<Item = &'static CatalogModel> + '_ {
    let needle = query.trim().to_ascii_lowercase();
    MODELS.iter().filter(move |model| {
        model.provider.contains(&needle)
            || model.id.contains(&needle)
            || model.label.to_ascii_lowercase().contains(&needle)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn entries_have_unique_provider_ids_and_published_limits() {
        let mut identities = HashSet::new();
        for model in models() {
            assert!(identities.insert((model.provider, model.id)));
            assert!(model.context_window > model.max_output_tokens);
            assert_eq!(model.checked_on, CHECKED_ON);
            assert!(model.source_url.starts_with("https://"));
            assert!(model.endpoint.starts_with("https://"));
            assert!(model.api_key_env.ends_with("_API_KEY"));
        }
    }

    #[test]
    fn lookup_is_exact_and_search_is_case_insensitive() {
        assert_eq!(
            find("openai", "gpt-5.4").map(|model| model.label),
            Some("GPT-5.4")
        );
        assert!(find("anthropic", "gpt-5.4").is_none());
        assert!(find("openai", "gpt-5").is_none());
        assert_eq!(
            search(" OPUS ").map(|model| model.id).collect::<Vec<_>>(),
            vec!["claude-opus-4-6"]
        );
        assert_eq!(search("anthropic").count(), 3);
        assert_eq!(
            find("deepseek", "deepseek-flash").map(|model| model.wire),
            Some(CatalogWire::DeepSeekChat)
        );
        assert_eq!(
            find("xiaomi", "mimo-v2.6-flash").map(|model| model.wire),
            Some(CatalogWire::MiMoChat)
        );
        assert_eq!(
            find("openrouter", "openai/gpt-5.4").map(|model| model.endpoint),
            Some(OPENROUTER_ENDPOINT)
        );
    }
}
