# Rust LLM Provider Crates Survey (2026-02)

**Research Date**: 2026-02-13
**Purpose**: Evaluate Rust crates for unified multi-provider LLM access with streaming and tool calling
**Requirements**: Streaming (SSE), tool/function calling, multi-provider (Anthropic, OpenAI, Google, etc.), tokio async

---

## Summary Table

| Crate             | Version             | Downloads (all / 90d) | Providers       | Streaming | Tool Calling        | Last Update | Verdict                          |
| ----------------- | ------------------- | --------------------- | --------------- | --------- | ------------------- | ----------- | -------------------------------- |
| **rig-core**      | 0.30.0              | 262k / 119k           | 20+             | Yes       | Yes                 | 2026-02-03  | Most popular, full framework     |
| **async-openai**  | 0.32.4              | 3.0M / 1.3M           | OpenAI + compat | Yes (SSE) | Yes                 | 2026-01-25  | Dominant for OpenAI protocol     |
| **genai**         | 0.5.3 (0.6.0-alpha) | 119k / 32k            | 14+ native      | Yes       | Yes (since v0.4.0)  | 2026-02-13  | Best thin multi-provider client  |
| **llm**           | 1.3.7               | 57k / 10k             | 12+             | Yes       | Yes                 | 2026-01-09  | Batteries-included, CLI + lib    |
| **misanthropic**  | 0.5.1               | 15k / 177             | Anthropic only  | Yes       | Yes                 | 2024-11-30  | Anthropic-specific, unmaintained |
| **misanthropy**   | 0.0.8               | 12k / 780             | Anthropic only  | Yes       | Yes                 | 2025-06-08  | Anthropic-specific, semi-active  |
| **llm-connector** | 0.5.13              | 9k / 1k               | 11+             | Yes       | Yes (OpenAI-compat) | 2026-01-03  | Chinese provider focus           |
| **multi-llm**     | 1.0.0               | 66 / 66               | 4               | Yes       | Unknown             | 2025-11-28  | Too new, minimal adoption        |

---

## Detailed Analysis

### 1. rig-core (0xPlaygrounds/rig)

**crates.io**: https://crates.io/crates/rig-core
**GitHub**: https://github.com/0xPlaygrounds/rig (stars: high, 40+ releases)
**Install**: `cargo add rig-core`

**Providers** (20+): Anthropic, Azure, Cohere, Deepseek, Galadriel, Gemini, Groq, Huggingface, Hyperbolic, Mira, Mistral, Moonshot, Ollama, OpenAI, OpenRouter, Perplexity, Together, Voyage AI, xAI

**Architecture**: Full agent framework with traits for CompletionModel, streaming, embeddings, vector stores, extractors. Builder pattern for agents. Has `CompletionModel` trait with both `completion()` and `stream()` methods.

**Streaming**: First-class. `StreamingCompletionResponse` trait. Per-provider SSE parsing.

**Tool calling**: Yes. `Tool` trait with `NAME`, `Args`, `Output`, `Error` associated types. Automatic JSON schema generation via schemars. Tools registered on agents via builder. Multi-turn tool orchestration built in.

**Strengths**:

- Most mature ecosystem (vector stores, RAG, extractors)
- MCP client support (rmcp integration)
- Strong community, active development
- Well-documented with guides for writing custom providers
- WASM support for core library

**Weaknesses**:

- Heavy framework -- pulls in a lot for "just" multi-provider chat
- Opinionated abstractions may conflict with custom agent loops
- 54% doc coverage on docs.rs (improving)
- Agent-centric design may be overkill if you just want provider abstraction

**Assessment**: Best choice if building a full agent framework from scratch. Overkill if you already have an agent loop and just need provider abstraction.

---

### 2. async-openai (64bit/async-openai)

**crates.io**: https://crates.io/crates/async-openai
**GitHub**: https://github.com/64bit/async-openai
**Install**: `cargo add async-openai`

**Providers**: OpenAI natively. Configurable for any OpenAI-compatible API (Azure, Groq, Together, local, etc.) via custom base URL and headers.

**Architecture**: Direct bindings to OpenAI API. Typed request/response structs. `Client<Config>` is generic over config for different endpoints.

**Streaming**: Yes, full SSE streaming support.

**Tool calling**: Yes, via OpenAI's native function/tool calling API. Community crate `openai-func-enums` for macro-based tool definitions.

**Strengths**:

- By far the most downloaded Rust LLM crate (3M+ downloads)
- Battle-tested, production-proven
- Complete OpenAI API coverage (chat, assistants, audio, images, etc.)
- Exponential backoff retry built in
- Microsoft Azure support out of the box

**Weaknesses**:

- OpenAI protocol only -- no native Anthropic Messages API
- Need to handle Anthropic's different streaming format separately
- Not truly multi-provider: Anthropic, Google Gemini need separate crates

**Assessment**: Gold standard for OpenAI-protocol APIs. Not useful for Anthropic's native Messages API or Google's Gemini API which have different wire formats.

---

### 3. genai (jeremychone/rust-genai)

**crates.io**: https://crates.io/crates/genai
**GitHub**: https://github.com/jeremychone/rust-genai (621 stars, 618 commits)
**Install**: `cargo add genai`

**Providers** (14+ native): OpenAI, Anthropic, Gemini, xAI, Ollama, Groq, DeepSeek, Cohere, Together, Fireworks, Nebius, Mimo, Zai (Zhipu), BigModel. Custom URL via `ServiceTargetResolver`.

**Architecture**: Thin adapter layer. `Client` with `exec_chat()` and `exec_chat_stream()`. Each provider has an `Adapter` implementation that maps to/from a common `ChatRequest`/`ChatResponse`. Static dispatch. Minimal dependencies.

**Streaming**: Yes, unified `EventSourceStream` and `WebStream` internally. Print helpers for streaming output.

**Tool calling**: YES -- added in v0.4.0, with streaming tool call support. Works across OpenAI, Anthropic, Gemini, Ollama adapters. Recent fixes for Anthropic tool call streaming and parameter-less tool calls. Active PRs for web search/fetch tool support.

**Strengths**:

- Clean, thin abstraction -- does not try to be an agent framework
- Best provider coverage for a focused chat client
- Active development (v0.6.0-alpha in progress as of today)
- 84% docs.rs coverage
- Reasoning/thinking support for DeepSeek R1, Gemini, Anthropic
- PDF and image support (multimodal)
- Small dependency footprint

**Weaknesses**:

- Pre-1.0, API still evolving (0.5.x -> 0.6.0-alpha breaking changes)
- Tool calling relatively new (v0.4.0, ~mid 2025)
- No embeddings vector store ecosystem
- No agent loop, RAG, or orchestration -- just the client

**Assessment**: Best fit for a project like ion that already has its own agent loop, tool system, and streaming infrastructure. Provides exactly the provider abstraction layer without framework overhead.

---

### 4. llm (graniet/llm)

**crates.io**: https://crates.io/crates/llm
**GitHub**: https://github.com/graniet/llm (stars: moderate, 17+ contributors)
**Install**: `cargo add llm --features "openai,anthropic,google"`

**Providers** (12+): OpenAI, Anthropic, Ollama, DeepSeek, xAI, Phind, Groq, Google, Cohere, Mistral, Hugging Face, ElevenLabs, OpenRouter, AWS Bedrock

**Architecture**: Builder pattern (`LLMBuilder`). Feature flags per provider. `ChatProvider` and `CompletionProvider` traits. Includes CLI tool.

**Streaming**: Yes, per-provider streaming examples (OpenAI, Anthropic, xAI, Google).

**Tool calling**: Yes. `tool_calling_example` and `unified_tool_calling_example` (multi-turn, multi-provider). Google-specific tool calling example.

**Strengths**:

- Batteries included (CLI, REST API server, multi-step chains, evaluations)
- Voice support (ElevenLabs, speech-to-text)
- AWS Bedrock support
- Feature-flag-based: only compile providers you need
- Multi-step chain orchestration

**Weaknesses**:

- Name collision with the old `llm` crate for local inference (pre-2025)
- Batteries-included approach = larger dependency tree
- Less focused than genai on just being a clean client library
- Documentation quality varies by provider

**Assessment**: Good all-in-one solution. More than needed for ion's use case, but the unified tool calling across providers is well-tested.

---

### 5. misanthropic (mdegans/misanthropic)

**crates.io**: https://crates.io/crates/misanthropic
**GitHub**: https://github.com/mdegans/misanthropic
**Install**: `cargo add misanthropic`

**Provider**: Anthropic only.

**Features**: Streaming, tool use, prompt caching, image support, markdown/HTML formatting, API key encryption in memory, input sanitization.

**Strengths**: Ergonomic Anthropic-specific API, security-conscious design.

**Weaknesses**: Last updated November 2024. Anthropic only. Low recent downloads (177/90d). Appears unmaintained.

**Assessment**: Not suitable -- single provider, possibly abandoned.

---

### 6. misanthropy (cortesi/misanthropy)

**crates.io**: https://crates.io/crates/misanthropy
**GitHub**: https://github.com/cortesi/misanthropy (35 stars)
**Install**: `cargo add misanthropy`

**Provider**: Anthropic only.

**Features**: Streaming, tool usage via schemars JSON schema, extended thinking, CLI tool.

**Strengths**: Clean strongly-typed tool interface with schemars.

**Weaknesses**: v0.0.8, last updated June 2025. Anthropic only.

**Assessment**: Not suitable -- single provider, early stage.

---

### 7. llm-connector (lipish/llm-connector)

**crates.io**: https://crates.io/crates/llm-connector
**GitHub**: https://github.com/lipish/llm-connector
**Install**: `cargo add llm-connector`

**Providers** (11+): OpenAI, Anthropic, Google, Aliyun, Zhipu, Ollama, Tencent, Volcengine, LongCat, Moonshot, DeepSeek

**Architecture**: Protocol/Provider separation. Clean type-safe interface.

**Streaming**: Yes, unified streaming with automatic tool_calls deduplication.

**Tool calling**: Yes, OpenAI-compatible function calling. Streaming tool calls supported.

**Strengths**: Strong Chinese cloud provider coverage. Clean protocol abstraction.

**Weaknesses**: Low adoption (9k downloads, 1k/90d). Many rapid-fire releases (50+ versions). Primarily Chinese provider ecosystem focus.

**Assessment**: Interesting architecture but low adoption. Chinese provider coverage is unique but not needed for ion.

---

## Recommendation for ion

ion already has a custom provider system (`src/provider/`) with:

- Anthropic native client (`anthropic/`)
- OpenAI-compatible client (`openai_compat/`) handling OpenAI, Google, Groq, Kimi, Ollama, OpenRouter
- Custom SSE streaming (`http/sse.rs`)
- Custom tool calling integration
- Provider registry with model discovery

**The custom approach is the right one for ion.** Here is why:

1. **No crate covers all needs cleanly.** genai is closest but still pre-1.0 with evolving APIs. rig-core is too heavy a framework.

2. **ion's streaming needs are specific.** The TUI requires direct control over SSE chunk handling for incremental markdown rendering. Generic streaming abstractions add a translation layer.

3. **Tool calling formats differ per provider.** ion already handles Anthropic's `tool_use`/`tool_result` blocks and OpenAI's `tool_calls` array natively. A unified crate would need the same provider-specific code underneath.

4. **Provider quirks need direct handling.** The `openai_compat/quirks.rs` module exists for a reason -- each "OpenAI-compatible" provider has subtle differences.

**If reconsidering in the future**, genai (v0.6+ stable) would be the strongest candidate for replacing the provider layer, as it provides the same thin-adapter architecture ion uses without framework overhead.

---

## Sources

- https://crates.io/crates/rig-core
- https://crates.io/crates/async-openai
- https://crates.io/crates/genai
- https://crates.io/crates/llm
- https://crates.io/crates/misanthropic
- https://crates.io/crates/misanthropy
- https://crates.io/crates/llm-connector
- https://crates.io/crates/multi-llm
- https://github.com/0xPlaygrounds/rig
- https://github.com/64bit/async-openai
- https://github.com/jeremychone/rust-genai
- https://github.com/graniet/llm
- https://docs.rig.rs/
