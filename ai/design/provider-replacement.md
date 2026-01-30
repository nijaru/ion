# Provider Layer Replacement

## Background

llm-connector has limitations blocking key features:

| Issue                  | Impact                          | Blocked Feature                |
| ---------------------- | ------------------------------- | ------------------------------ |
| No `cache_control`     | Can't cache system prompts      | 50-100x cost savings (tk-268g) |
| No `provider` field    | Can't route OpenRouter requests | ProviderPrefs (already built)  |
| Parsing failures       | Kimi on OpenRouter broken       | tk-1lso                        |
| No `reasoning_content` | Can't extract thinking          | Extended thinking display      |

## Decision

Replace llm-connector with native HTTP implementations.

## Architecture

```
src/provider/
├── mod.rs              # Re-exports, Provider enum
├── api_provider.rs     # Provider metadata (existing)
├── client.rs           # LlmApi trait + Client (refactor)
├── http.rs             # NEW: Shared HTTP + SSE utilities
├── anthropic.rs        # NEW: Anthropic Messages API
├── openai_compat.rs    # NEW: OpenAI-compatible (OR, Groq, Kimi, OpenAI)
├── google.rs           # NEW or keep llm-connector for Gemini
├── types.rs            # Message, ContentBlock, etc (existing)
└── error.rs            # Error types (existing)
```

## API Formats

### Anthropic Messages API

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 8192,
  "system": [
    {
      "type": "text",
      "text": "System prompt...",
      "cache_control": {"type": "ephemeral"}  // NEW
    }
  ],
  "messages": [...],
  "tools": [...]
}
```

### OpenAI-Compatible (OpenRouter, Groq, Kimi)

```json
{
  "model": "anthropic/claude-sonnet-4",
  "messages": [...],
  "tools": [...],
  "provider": {  // NEW: OpenRouter routing
    "order": ["Anthropic"],
    "allow_fallbacks": false
  }
}
```

## Implementation Plan

### Phase 1: HTTP Foundation

- [ ] Create `http.rs` with:
  - `HttpClient` wrapper around reqwest
  - SSE stream parsing
  - Retry logic with backoff
  - Error categorization

### Phase 2: Anthropic Native

- [ ] Create `anthropic.rs` with:
  - Messages API request/response types
  - `cache_control` support on system messages
  - Streaming with content blocks
  - Thinking extraction

### Phase 3: OpenAI-Compatible

- [ ] Create `openai_compat.rs` with:
  - Chat completions request/response types
  - `provider` field for OpenRouter
  - Tool calling support
  - Streaming delta handling

### Phase 4: Client Refactor

- [ ] Update `client.rs` to route to new implementations
- [ ] Remove llm-connector dependency
- [ ] Update Cargo.toml

### Phase 5: Google (Optional)

- [ ] Evaluate if llm-connector works for Gemini
- [ ] If not, implement native Google AI Studio client

## Response Types

### Anthropic Streaming Events

```
event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: message_stop
data: {"type":"message_stop"}
```

### OpenAI Streaming Events

```
data: {"id":"...","choices":[{"delta":{"content":"Hello"}}]}

data: [DONE]
```

## Testing Strategy

1. Unit tests for request/response serialization
2. Integration tests with mock server (wiremock)
3. Manual testing against real APIs

## Rollout

1. Implement behind feature flag initially
2. Test with each provider
3. Remove llm-connector once stable

## References

- Anthropic API: https://docs.anthropic.com/en/api/messages
- OpenAI API: https://platform.openai.com/docs/api-reference/chat
- OpenRouter: https://openrouter.ai/docs
