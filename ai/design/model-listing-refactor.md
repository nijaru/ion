# Model Listing Architecture Refactor

## Problem

Current design has issues:

1. **Duplication**: OpenRouter model fetching in both `Client` and `ModelRegistry`
2. **Wrong responsibility**: `Client` does model listing (metadata) when it should only do LLM calls
3. **Unused trait method**: `LlmApi::list_models()` returns empty for most backends

## Current Flow

```
ModelPicker
    -> fetch_models_for_picker()
        -> registry.fetch_hybrid(provider)
            -> provider.list_models()    <-- Client impl (duplicates registry)
            -> models.dev metadata       <-- hydration
```

## Proposed Design

### Principle: Separation of Concerns

| Component       | Responsibility               |
| --------------- | ---------------------------- |
| `Client`        | LLM API calls (chat, stream) |
| `ModelRegistry` | Model discovery and metadata |

### Key Insight

Model listing has two distinct sources:

| Provider Type                               | Metadata Source   | Availability Fetcher       |
| ------------------------------------------- | ----------------- | -------------------------- |
| Cloud (Anthropic, Groq, Google, OpenAI)     | `charm.land/catwalk` | Provider API (if possible) |
| Multi-Provider (OpenRouter)                 | API (Live)        | API (Live)                 |
| Local (Ollama)                              | API (Live)        | Local server API           |

### Key Insight: Static vs Dynamic

1. **Static Metadata:** Context windows, pricing, and stable model names are sourced from `catwalk` at compile-time/start-time. This eliminates the need for an external "hydration" API like `models.dev` for basic specs.
2. **Dynamic Availability:** The registry queries provider endpoints (e.g., `GET /api/tags` for Ollama) to see which specific models are *currently* available to the user.
3. **Fallback:** If a model is found in the API but not in `catwalk`, use a default configuration and flag it for manual update.

### Changes

#### 1. Remove `list_models` from `LlmApi` trait

```rust
// Before
pub trait LlmApi: Send + Sync {
    fn id(&self) -> &str;
    fn model_info(&self, model_id: &str) -> Option<ModelInfo>;
    fn models(&self) -> Vec<ModelInfo>;
    async fn list_models(&self) -> Result<Vec<ModelInfo>, Error>;  // REMOVE
    async fn stream(...) -> Result<(), Error>;
    async fn complete(...) -> Result<Message, Error>;
}

// After
pub trait LlmApi: Send + Sync {
    fn id(&self) -> &str;
    async fn stream(...) -> Result<(), Error>;
    async fn complete(...) -> Result<Message, Error>;
}
```

Also remove `model_info()` and `models()` - unused.

#### 2. Move backend-specific fetching to `ModelRegistry`

```rust
impl ModelRegistry {
    /// Fetch models for the given backend
    pub async fn fetch_models_for_backend(&self, backend: Backend) -> Result<Vec<ModelInfo>> {
        match backend {
            Backend::Ollama => self.fetch_ollama_models().await,
            Backend::OpenRouter => self.fetch_openrouter_models().await,
            // Cloud providers: use models.dev or static knowledge
            _ => self.fetch_from_models_dev(backend).await,
        }
    }

    async fn fetch_ollama_models(&self) -> Result<Vec<ModelInfo>> {
        // Move from Client::list_ollama_models
        let response = self.client
            .get("http://localhost:11434/api/tags")
            .send().await?;
        // ... parse response
    }

    async fn fetch_openrouter_models(&self) -> Result<Vec<ModelInfo>> {
        // Already exists, keep as-is
    }

    async fn fetch_from_models_dev(&self, backend: Backend) -> Result<Vec<ModelInfo>> {
        // Filter models.dev results by provider
        let all = models_dev::fetch_models_dev().await?;
        Ok(all.into_iter()
            .filter(|m| matches_backend(&m.provider, backend))
            .collect())
    }
}
```

#### 3. Update model picker to pass backend instead of provider

```rust
// Before
pub async fn fetch_models_for_picker(
    registry: &ModelRegistry,
    provider: Arc<dyn LlmApi>,  // <-- provider
    prefs: &ProviderPrefs,
) -> Result<Vec<ModelInfo>> {
    let models = registry.fetch_hybrid(provider).await?;
    // ...
}

// After
pub async fn fetch_models_for_picker(
    registry: &ModelRegistry,
    backend: Backend,  // <-- backend enum
    prefs: &ProviderPrefs,
) -> Result<Vec<ModelInfo>> {
    let models = registry.fetch_models_for_backend(backend).await?;
    // ...
}
```

#### 4. Remove `fetch_hybrid` from registry

No longer needed - direct fetching handles everything.

### Files to Change

| File                       | Changes                                                                      |
| -------------------------- | ---------------------------------------------------------------------------- |
| `src/provider/client.rs`   | Remove `list_models`, `list_ollama_models`, `list_openrouter_models`         |
| `src/provider/mod.rs`      | Remove `list_models` from `LlmApi` trait                                     |
| `src/provider/registry.rs` | Add `fetch_models_for_backend`, `fetch_ollama_models`, remove `fetch_hybrid` |
| `src/tui/mod.rs`           | Pass `self.api_provider.to_backend()` instead of provider                    |
| `src/tui/model_picker.rs`  | Update `fetch_models_for_picker` signature                                   |

### Benefits

1. **Single responsibility**: Client does LLM calls, Registry does metadata
2. **No duplication**: One place for OpenRouter fetching
3. **Simpler trait**: `LlmApi` only has methods Client needs
4. **Clearer ownership**: Registry owns all model discovery logic

### Migration Path

1. Add `fetch_models_for_backend` to registry (copy code from client)
2. Update model picker to use new method
3. Remove old methods from client
4. Remove `list_models` from trait
5. Clean up `fetch_hybrid`

### Testing

- Verify OpenRouter model listing still works
- Verify Ollama model listing still works
- Verify model picker shows correct models after provider switch
