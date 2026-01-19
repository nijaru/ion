# Rust LLM API Crates Research

**Date:** 2026-01-19
**Context:** ion uses `llm` crate v1.3.7 for multi-provider LLM access

## Summary

| Crate             | Streaming | Tool Calls | Multi-Provider | Downloads/mo | Recommendation     |
| ----------------- | --------- | ---------- | -------------- | ------------ | ------------------ |
| **llm** (graniet) | Yes       | Yes        | Yes (12+)      | 1,698        | **Current choice** |
| async-openai      | Yes       | Yes        | OpenAI only    | 69,022       | OpenAI-specific    |
| genai             | Yes       | Planned    | Yes (12+)      | ~1,000       | Watch for tools    |
| anthropic         | No        | No         | Anthropic only | 600          | Avoid (stale)      |
| claudius          | Yes       | Yes        | Anthropic only | 123          | Anthropic-specific |

## Current: llm crate (graniet/llm)

**Version:** 1.3.7 (ion uses this)
**Source:** https://github.com/graniet/llm

### What it provides

- Multi-backend: OpenAI, Anthropic, Ollama, DeepSeek, xAI, Groq, Google, Cohere, Mistral
- Unified builder pattern API
- Streaming via `chat_stream_with_tools()`
- Tool/function calling with `ToolCall` struct
- OpenRouter support via OpenAI-compatible feature

### Current ion integration

```rust
// src/provider/client.rs
let llm = LLMBuilder::new()
    .backend(self.backend.to_llm())
    .model(model)
    .api_key(&self.api_key)
    .function(func)  // Tool registration
    .build()?;

// Streaming with tools
let mut stream = llm.chat_stream_with_tools(&messages, tools_ref).await?;
```

### Limitations encountered

1. **System message handling**: Converted to user messages with `[System]:` prefix
2. **Tool result format**: Manual formatting as text in user messages
3. **Provider variations**: Some normalization needed for different backends
4. **Documentation**: 67% documented, some gaps

### Dependencies it brings

```toml
[dependencies]
async-trait = "0.1"
futures = "0.3"
reqwest = "0.12"
serde = "1.0"
serde_json = "1.0"
tokio = "1.0"
```

## Alternative: async-openai

**Version:** 0.32.3
**Source:** https://github.com/64bit/async-openai
**Downloads:** 69,022/month (most popular)

### Pros

- Most complete OpenAI API coverage
- Excellent documentation (91 code snippets)
- Full tool/function calling support
- Native streaming with SSE
- Realtime API support
- Well-maintained (26 days ago)

### Cons

- **OpenAI only** - no Anthropic, Google, etc.
- Would need separate crates for other providers
- Heavier dependency set

### When to use

- OpenAI-only projects
- Need complete OpenAI API (files, assistants, batches)
- OpenAI-compatible endpoints (vLLM, Ollama OpenAI mode)

## Alternative: genai (jeremychone)

**Version:** 0.5.x
**Source:** https://github.com/jeremychone/rust-genai
**Stars:** 617

### Pros

- Native multi-provider: OpenAI, Anthropic, Gemini, xAI, Ollama, Groq, DeepSeek, Cohere, Together, Fireworks
- Clean ergonomic API
- Good streaming support
- Active development (v0.5.0 Jan 2026)
- Native protocol support (Gemini/Anthropic reasoning)
- Model auto-detection from name prefix

### Cons

- **No tool calling yet** (planned per README: "function calling coming later")
- Missing for coding agents
- Smaller community than async-openai

### Example API

```rust
use genai::chat::{ChatMessage, ChatRequest};
use genai::Client;

let client = Client::default();
let chat_req = ChatRequest::new(vec![
    ChatMessage::system("Answer in one sentence"),
    ChatMessage::user("Why is the sky blue?"),
]);

// Non-streaming
let response = client.exec_chat("gpt-4o", chat_req.clone(), None).await?;

// Streaming
let stream = client.exec_chat_stream("claude-3-haiku", chat_req, None).await?;
```

## Provider-Specific Crates

### anthropic / anthropic-api

- **anthropic** (0.0.8): Stale, 2021 edition, no streaming
- **anthropic-api** (0.0.5): Basic, streaming via reqwest-eventsource
- **claudius** (0.11.0): New, agent framework included, but Anthropic-only
- **async-anthropic** (bosun-ai): Streaming + async, but limited adoption

**Recommendation:** Use llm crate's Anthropic support instead

### openai-rust / openai-tools

- Several exist, all inferior to async-openai
- async-openai is the clear winner for OpenAI-specific

## What we'd need to build without llm crate

If rolling our own:

1. **HTTP client setup** - reqwest with JSON + stream features
2. **SSE parsing** - reqwest-eventsource or eventsource-stream
3. **Per-provider message formats** - Different tool call schemas
4. **Streaming chunk handling** - Different chunk formats per provider
5. **Error normalization** - Provider-specific errors to unified errors

Estimate: 500-1000 LOC per provider for streaming + tools

## Recommendation

**Keep llm crate** for now. Reasons:

1. Already working in ion with streaming + tools
2. Covers all target providers (OpenRouter, Anthropic, OpenAI, Ollama, Groq, Google)
3. Maintained (1.3.7 released recently)
4. Reasonable abstraction overhead

**Watch genai** for future consideration:

- When it adds tool calling
- Cleaner API design than llm
- Better native provider protocol support

**Consider async-openai** if:

- Need deep OpenAI API features (assistants, files, etc.)
- Using only OpenAI-compatible endpoints

## Specific Gaps in llm crate

Current pain points in ion's `/src/provider/client.rs`:

1. **System messages as user messages** (line 111-119)
2. **Tool results formatted as text** (line 156-171)
3. **Thinking blocks not natively handled** (line 141-144)
4. **No native ContentBlock types** - ion defines its own

These are manageable with our abstraction layer but worth noting.

## Future Considerations

1. **Anthropic tool_use blocks** - May need custom handling for Claude's native format
2. **Extended thinking** - Anthropic's thinking blocks need special streaming
3. **OpenRouter specifics** - Provider routing headers not exposed by llm crate
4. **Context caching** - Provider-specific, may need direct API calls

## References

- llm crate: https://docs.rs/llm/latest/llm/
- async-openai: https://docs.rs/async-openai/latest/async_openai/
- genai: https://github.com/jeremychone/rust-genai
- ion provider code: `/Users/nick/github/nijaru/ion/src/provider/client.rs`
