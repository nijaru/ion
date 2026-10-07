//! Curated starting points for the model wire APIs Ion currently implements.
//!
//! Checked 2026-09-26, with current Claude and OpenAI models refreshed 2026-09-29,
//! against the linked provider model pages. A context window
//! includes input and output tokens; it is not an independent input allowance.
//! Catalog membership says the wire route exists, not that an account has access
//! to the model or that a live request has been qualified.
//! Claude 5 models emit signed thinking by default. The Messages route retains
//! their blocks and rebases changed prefixes; native live access still depends
//! on an Anthropic credential and account entitlement.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogWire {
    ChatCompletions,
    DeepSeekChat,
    MiMoChat,
    OpenRouterChat,
    AnthropicMessages,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptCacheLifetime {
    /// The route may cache, but Ion has no verified retention contract for it.
    Unknown,
    /// The provider manages retention and the exact lifetime is not a stable
    /// route contract Ion should assume.
    ProviderManaged,
    /// Verified minimum/default cache lifetimes for this model route.
    Fixed {
        default_seconds: u32,
        extended_seconds: Option<u32>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PromptCachePricing {
    /// Micro-US-dollars per million tokens.
    pub write_5m_microusd_per_million: u64,
    pub read_microusd_per_million: u64,
    pub output_microusd_per_million: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptCacheRefresh {
    None,
    /// Replay the exact last request with a one-token output ceiling.
    ReplayOneToken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PromptCacheCapabilities {
    /// Whether this route is expected to report cache read/write token detail.
    pub usage_details: bool,
    pub lifetime: PromptCacheLifetime,
    /// Whether the adapter must explicitly opt reusable requests into
    /// provider prompt caching.
    pub automatic_request: bool,
    /// Verified refresh mechanism for keeping the active prefix alive.
    pub refresh: PromptCacheRefresh,
    /// Current direct-route prices used only for economic warming decisions.
    pub pricing: Option<PromptCachePricing>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextMutationCapabilities {
    pub mid_conversation_system: bool,
    pub mid_conversation_tools: bool,
    pub inline_tool_definitions: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelCapabilities {
    pub prompt_cache: PromptCacheCapabilities,
    pub context_mutation: ContextMutationCapabilities,
}

impl ModelCapabilities {
    pub const fn conservative() -> Self {
        Self {
            prompt_cache: PromptCacheCapabilities {
                usage_details: false,
                lifetime: PromptCacheLifetime::Unknown,
                automatic_request: false,
                refresh: PromptCacheRefresh::None,
                pricing: None,
            },
            context_mutation: ContextMutationCapabilities {
                mid_conversation_system: false,
                mid_conversation_tools: false,
                inline_tool_definitions: false,
            },
        }
    }
}

const OPENAI_MANAGED_CACHE: ModelCapabilities = ModelCapabilities {
    prompt_cache: PromptCacheCapabilities {
        usage_details: true,
        lifetime: PromptCacheLifetime::ProviderManaged,
        automatic_request: false,
        refresh: PromptCacheRefresh::None,
        pricing: None,
    },
    context_mutation: ContextMutationCapabilities {
        mid_conversation_system: false,
        mid_conversation_tools: false,
        inline_tool_definitions: false,
    },
};

const OPENROUTER_PROVIDER_CACHE: ModelCapabilities = ModelCapabilities {
    prompt_cache: PromptCacheCapabilities {
        usage_details: true,
        lifetime: PromptCacheLifetime::ProviderManaged,
        automatic_request: false,
        refresh: PromptCacheRefresh::None,
        pricing: None,
    },
    context_mutation: ContextMutationCapabilities {
        mid_conversation_system: false,
        mid_conversation_tools: false,
        inline_tool_definitions: false,
    },
};

const ANTHROPIC_CACHE_ONLY: ModelCapabilities = ModelCapabilities {
    prompt_cache: PromptCacheCapabilities {
        usage_details: true,
        lifetime: PromptCacheLifetime::Fixed {
            default_seconds: 300,
            extended_seconds: Some(3600),
        },
        automatic_request: true,
        refresh: PromptCacheRefresh::ReplayOneToken,
        pricing: None,
    },
    context_mutation: ContextMutationCapabilities {
        mid_conversation_system: false,
        mid_conversation_tools: false,
        inline_tool_definitions: false,
    },
};

const fn anthropic_inline_context(pricing: PromptCachePricing) -> ModelCapabilities {
    ModelCapabilities {
        prompt_cache: PromptCacheCapabilities {
            usage_details: true,
            lifetime: PromptCacheLifetime::Fixed {
                default_seconds: 300,
                extended_seconds: Some(3600),
            },
            automatic_request: true,
            refresh: PromptCacheRefresh::ReplayOneToken,
            pricing: Some(pricing),
        },
        context_mutation: ContextMutationCapabilities {
            mid_conversation_system: true,
            mid_conversation_tools: true,
            inline_tool_definitions: true,
        },
    }
}

const ANTHROPIC_OPUS_5_5: ModelCapabilities = anthropic_inline_context(PromptCachePricing {
    write_5m_microusd_per_million: 5_000_000,
    read_microusd_per_million: 200_000,
    output_microusd_per_million: 20_000_000,
});
const ANTHROPIC_SONNET_5_5: ModelCapabilities = anthropic_inline_context(PromptCachePricing {
    write_5m_microusd_per_million: 2_500_000,
    read_microusd_per_million: 200_000,
    output_microusd_per_million: 10_000_000,
});
const ANTHROPIC_FABLE_5_1: ModelCapabilities = anthropic_inline_context(PromptCachePricing {
    write_5m_microusd_per_million: 12_500_000,
    read_microusd_per_million: 250_000,
    output_microusd_per_million: 50_000_000,
});

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
    /// Whether the current model page declares image input on this route.
    pub image_input: bool,
    pub capabilities: ModelCapabilities,
    pub source_url: &'static str,
    pub checked_on: &'static str,
}

const CHECKED_ON: &str = "2026-09-26";
const CURRENT_CHECKED_ON: &str = "2026-09-29";
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
        image_input: true,
        capabilities: ModelCapabilities::conservative(),
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
        image_input: true,
        capabilities: ModelCapabilities::conservative(),
        source_url: "https://mimo.mi.com/models/en-US/mimo-v2.6-flash",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "openrouter",
        id: "deepseek/deepseek-v4.1-flash",
        label: "DeepSeek V4.1 Flash via OpenRouter",
        wire: CatalogWire::OpenRouterChat,
        endpoint: OPENROUTER_ENDPOINT,
        api_key_env: "OPENROUTER_API_KEY",
        context_window: 1_048_576,
        max_output_tokens: 128_000,
        image_input: true,
        capabilities: OPENROUTER_PROVIDER_CACHE,
        source_url: "https://openrouter.ai/deepseek/deepseek-v4.1-flash/providers",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "openai",
        id: "gpt-5.5",
        label: "GPT-5.5",
        wire: CatalogWire::ChatCompletions,
        endpoint: OPENAI_ENDPOINT,
        api_key_env: "OPENAI_API_KEY",
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        image_input: true,
        capabilities: OPENAI_MANAGED_CACHE,
        source_url: "https://developers.openai.com/api/docs/models/gpt-5.5",
        checked_on: CURRENT_CHECKED_ON,
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
        image_input: true,
        capabilities: OPENAI_MANAGED_CACHE,
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
        image_input: true,
        capabilities: OPENAI_MANAGED_CACHE,
        source_url: "https://developers.openai.com/api/docs/models/gpt-5.4",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "openrouter",
        id: "openai/gpt-5.4",
        label: "GPT-5.4 via OpenRouter",
        wire: CatalogWire::OpenRouterChat,
        endpoint: OPENROUTER_ENDPOINT,
        api_key_env: "OPENROUTER_API_KEY",
        context_window: 1_050_000,
        max_output_tokens: 128_000,
        image_input: true,
        capabilities: OPENROUTER_PROVIDER_CACHE,
        source_url: "https://openrouter.ai/openai/gpt-5.4",
        checked_on: CHECKED_ON,
    },
    CatalogModel {
        provider: "anthropic",
        id: "claude-opus-5-5",
        label: "Claude Opus 5.5",
        wire: CatalogWire::AnthropicMessages,
        endpoint: ANTHROPIC_ENDPOINT,
        api_key_env: "ANTHROPIC_API_KEY",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        image_input: true,
        capabilities: ANTHROPIC_OPUS_5_5,
        source_url: "https://platform.claude.com/docs/en/models/opus-5-5/overview",
        checked_on: CURRENT_CHECKED_ON,
    },
    CatalogModel {
        provider: "anthropic",
        id: "claude-sonnet-5-5",
        label: "Claude Sonnet 5.5",
        wire: CatalogWire::AnthropicMessages,
        endpoint: ANTHROPIC_ENDPOINT,
        api_key_env: "ANTHROPIC_API_KEY",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        image_input: true,
        capabilities: ANTHROPIC_SONNET_5_5,
        source_url: "https://platform.claude.com/docs/en/models/sonnet-5-5/overview",
        checked_on: CURRENT_CHECKED_ON,
    },
    CatalogModel {
        provider: "anthropic",
        id: "claude-fable-5-1",
        label: "Claude Fable 5.1",
        wire: CatalogWire::AnthropicMessages,
        endpoint: ANTHROPIC_ENDPOINT,
        api_key_env: "ANTHROPIC_API_KEY",
        context_window: 1_000_000,
        max_output_tokens: 128_000,
        image_input: true,
        capabilities: ANTHROPIC_FABLE_5_1,
        source_url: "https://platform.claude.com/docs/en/models/fable-5-1/overview",
        checked_on: CURRENT_CHECKED_ON,
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
        image_input: true,
        capabilities: ANTHROPIC_CACHE_ONLY,
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
        image_input: true,
        capabilities: ANTHROPIC_CACHE_ONLY,
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
        image_input: true,
        capabilities: ANTHROPIC_CACHE_ONLY,
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
            assert!(matches!(model.checked_on, CHECKED_ON | CURRENT_CHECKED_ON));
            assert!(model.source_url.starts_with("https://"));
            assert!(model.endpoint.starts_with("https://"));
            assert!(model.api_key_env.ends_with("_API_KEY"));
        }
    }

    #[test]
    fn lookup_is_exact() {
        assert_eq!(
            find("openai", "gpt-5.4").map(|model| model.label),
            Some("GPT-5.4")
        );
        assert!(find("anthropic", "gpt-5.4").is_none());
        assert!(find("openai", "gpt-5").is_none());
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
