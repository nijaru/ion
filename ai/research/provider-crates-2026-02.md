# Rust LLM Provider & Agent Crates Research

**Date:** 2026-02-11
**Context:** Ion has ~8,900 lines of custom provider code covering 3 protocol families (Anthropic, OpenAI-compat, Google) across 7+ providers, with streaming, tool calling, OAuth/subscription auth, thinking blocks, and provider quirks. Evaluating whether external crates can reduce maintenance.
**Prior research:** `ai/research/rust-llm-crates-2026.md` (2026-01-19, now outdated)
**Related:** `ai/design/runtime-stack-integration-plan-2026-02.md` (Phase 3: genai adapter)

## Provider/Client Crates

### Comparison Matrix

| Crate                          | Version | Downloads (total/recent) | Providers                                                                                                              | Streaming                   | Tool Call Streaming                                                 | Token Usage           | Custom Auth                                                    | Last Updated |
| ------------------------------ | ------- | ------------------------ | ---------------------------------------------------------------------------------------------------------------------- | --------------------------- | ------------------------------------------------------------------- | --------------------- | -------------------------------------------------------------- | ------------ |
| **genai**                      | 0.5.3   | 117K / ~5K/mo            | 14+ (OpenAI, Anthropic, Gemini, xAI, Ollama, Groq, DeepSeek, Cohere, Together, Fireworks, Nebius, Mimo, Zai, BigModel) | Yes (SSE)                   | Yes (`ToolCallChunk`)                                               | Yes (`capture_usage`) | Yes (`AuthResolver`, `ServiceTargetResolver`, `extra_headers`) | 2026-01-31   |
| **async-openai**               | 0.32.4  | 2.9M / ~240K/mo          | OpenAI + Azure + OpenAI-compat                                                                                         | Yes (SSE)                   | Yes (full)                                                          | Yes                   | Config trait (custom base URL, headers)                        | 2026-01-25   |
| **rig-core**                   | 0.30.0  | 255K / ~114K/mo          | 20+ via built-in + companion crates                                                                                    | Yes (`StreamingCompletion`) | Partial (tool calls collected complete, not streamed incrementally) | Yes (in stream end)   | Per-provider Client config                                     | 2026-02-03   |
| **llm** (graniet)              | 1.3.7   | 57K / ~10K/mo            | 12+ (OpenAI, Anthropic, Ollama, DeepSeek, xAI, Groq, Google, Cohere, Mistral, OpenRouter)                              | Yes                         | Yes                                                                 | Partial               | API key only                                                   | 2026-01-09   |
| **llm-connector**              | 0.5.13  | 9K / ~1K/mo              | 11+ (OpenAI, Anthropic, Google, Aliyun, Zhipu, Ollama, Tencent, Volcengine, etc.)                                      | Yes (feature-gated)         | Unclear                                                             | Unclear               | Provider config struct                                         | 2026-01-03   |
| **ollama-rs**                  | 0.3.3   | 223K / ~20K/mo           | Ollama only                                                                                                            | Yes                         | Yes (Coordinator pattern)                                           | Yes                   | N/A (local)                                                    | 2025-11-18   |
| **claudius**                   | 0.18.0  | 8K / ~1.4K/mo            | Anthropic only                                                                                                         | Yes                         | Yes                                                                 | Yes                   | API key + custom base URL                                      | 2026-01-26   |
| **async-anthropic** (bosun-ai) | 0.6.0   | ~1K                      | Anthropic only                                                                                                         | Yes                         | Yes                                                                 | Yes                   | Builder config                                                 | 2025-05      |
| **anthropic-sdk-rust**         | varies  | ~2K                      | Anthropic only                                                                                                         | Yes                         | Yes                                                                 | Yes                   | API key                                                        | varies       |
| **multi-llm**                  | 1.0.0   | 45                       | OpenAI, Anthropic, Ollama, LMStudio                                                                                    | Yes                         | Unclear                                                             | Unclear               | API key                                                        | 2026-02      |

### Detailed Evaluations

#### genai (Jeremy Chone) -- PRIMARY CANDIDATE

**Strengths:**

- Closest philosophical match to Ion: "ergonomics and commonality, depth secondary"
- Native implementation (no per-provider SDK deps), similar to Ion's approach
- Full streaming with `ChatStreamEvent` enum: `Chunk`, `ReasoningChunk`, `ToolCallChunk`, `ThoughtSignatureChunk`
- Tool calling: `Tool` struct with JSON Schema, `ToolCall` response type, `ToolChunk` in streaming
- `ChatOptions.capture_tool_calls` for stream-based tool accumulation
- `ChatRequest.append_tool_use_from_stream_end()` for multi-turn tool loops
- `AuthResolver` and `ServiceTargetResolver` for custom auth flows
- `extra_headers` in ChatOptions for per-request customization
- `reasoning_content` support (DeepSeek R1, thinking blocks)
- Provider coverage overlaps well with Ion (missing: Kimi -- but addressable via `ServiceTargetResolver` custom URL)
- Actively maintained by one developer (Jeremy Chone), strong Rust community presence

**Weaknesses:**

- Single-maintainer project (bus factor)
- No OAuth flow support (Ion needs this for ChatGPT, Gemini subscription providers)
- No `cache_control` support (Anthropic-specific, Ion uses this heavily)
- `capture_usage` may not expose cache read/write token granularity Ion tracks
- Stream event model is slightly different from Ion's (`StreamEvent::ToolCall` vs genai's chunk-based accumulation)
- 14 providers but not all match Ion's list (no OpenRouter as named provider, though achievable via target resolver)

**Fit assessment:** Good adapter candidate for API-key providers. Cannot replace Ion's Anthropic-specific `cache_control`, subscription/OAuth providers, or provider quirks layer. Aligns with Phase 3 plan in runtime integration doc.

#### async-openai -- MATURE, NARROW

**Strengths:**

- By far the most downloaded Rust LLM crate (240K/mo)
- Complete OpenAI API coverage including Responses API, Realtime, Batch
- Excellent streaming + tool call streaming support
- Dynamic dispatch via `Config` trait for multi-provider OpenAI-compat usage
- Azure support built-in
- Well-tested, 91 contributors

**Weaknesses:**

- OpenAI-only by design (no Anthropic, no Google native)
- OpenAI-compat providers work but with no quirk handling
- Types are OpenAI-specific (would need mapping layer anyway)
- Very large API surface (20K SLoC) -- overkill for chat-only usage

**Fit assessment:** Only useful if Ion wanted to offload OpenAI-specific protocol handling. Since Ion already handles OpenAI-compat with quirks for 5+ providers, the mapping cost is not worth the dependency.

#### llm (graniet) -- DECLINING RELEVANCE

**Strengths:**

- Broad provider coverage, builder pattern
- Tool calling support across providers
- Active development

**Weaknesses:**

- Crate name collision (previously belonged to rustformers/llm for local inference)
- Smaller community than genai or rig
- Less sophisticated streaming model
- No custom auth/resolver infrastructure

**Fit assessment:** Superseded by genai for Ion's purposes. No advantage over current custom code.

#### llm-connector -- IMMATURE

**Strengths:**

- Clean protocol/provider separation concept
- Good Chinese provider coverage (Aliyun, Zhipu, Tencent, Volcengine)

**Weaknesses:**

- Very low adoption (1K/mo)
- Streaming is feature-gated, unclear tool call streaming support
- Docs are 72% covered
- No evidence of production usage

**Fit assessment:** Not ready. Skip.

#### Provider-specific crates (ollama-rs, claudius, async-anthropic)

These are useful reference implementations but adopting single-provider crates means N dependencies instead of 1 abstraction. Ion already handles each protocol natively. Only worth considering if abandoning the unified provider layer entirely.

## Agent/Conversation Framework Crates

### Comparison Matrix

| Crate                   | Version | Downloads (total/recent) | Agent Loop                 | Tool Orchestration            | Session Persistence | Streaming               | Conversation Mgmt             | Last Updated |
| ----------------------- | ------- | ------------------------ | -------------------------- | ----------------------------- | ------------------- | ----------------------- | ----------------------------- | ------------ |
| **rig-core**            | 0.30.0  | 255K / 114K/mo           | Yes (Agent, StreamingChat) | Yes (ToolSet, ToolSetBuilder) | No (bring your own) | Yes                     | Yes (Chat trait with history) | 2026-02-03   |
| **swarms-rs**           | 0.2.1   | 4K / 252/mo              | Yes (Agent with max_loops) | MCP-based only                | Autosave to disk    | No streaming agent loop | Multi-agent orchestration     | 2025-09-08   |
| **langchain-rust**      | 4.6.0   | 128K / 12K/mo            | Yes (Agent types)          | Yes (tool traits)             | Chain-based         | Yes                     | Chain/agent patterns          | 2024-10-06   |
| **claude-agent-sdk-rs** | 0.6.4   | 4K / ~1K/mo              | Yes (Claude Code-style)    | Yes (hooks, custom tools)     | No                  | Bidirectional streaming | Claude Code-specific          | 2026-02-09   |

### Detailed Evaluations

#### rig-core (0xPlaygrounds) -- MOST MATURE FRAMEWORK

**Strengths:**

- Largest Rust AI framework by adoption (5.7K GitHub stars, 114K downloads/mo)
- Clean trait hierarchy: `CompletionModel` -> `Agent` -> `StreamingPrompt`/`StreamingChat`
- `Tool` trait with derive macro for ergonomic tool definition
- `ToolSet` for managing collections of tools with optional RAG lookup
- 20+ provider integrations (built-in + companion crates)
- MCP support via `rmcp` integration
- WASM support planned
- Active development, multiple contributors

**Weaknesses:**

- Framework-level abstraction -- adopting it means conforming to rig's Agent/Completion model
- Streaming tool calls are collected complete (not incrementally streamed to caller)
- Would require significant adapter code to bridge rig types <-> Ion canonical types
- Brings substantial dependency tree (~5-26MB, 278K SLoC transitive)
- No session persistence built-in
- Agent loop is opinionated -- Ion's decomposed phase model (response -> tool -> state) would need to wrap or replace rig's loop

**Fit assessment:** Too much framework for Ion to adopt wholesale. The `Tool` trait and `ToolSet` patterns are worth studying for design inspiration. The `CompletionModel` trait could theoretically be implemented for Ion's providers, but the mapping cost is high and Ion gains little since it already has a working agent loop. **Do not adopt now** (aligns with runtime integration plan).

#### swarms-rs -- EARLY STAGE

**Strengths:**

- Multi-agent orchestration focus
- MCP integration for tools

**Weaknesses:**

- Very low adoption (252 downloads/mo), last updated Sep 2025
- No streaming agent loop
- OpenAI/DeepSeek only for providers
- "Coming soon" on function calling, custom tools, memory plugins
- Hype-driven marketing ("bleeding-edge", "enterprise-grade") with thin implementation

**Fit assessment:** Not relevant to Ion. Skip.

#### langchain-rust -- STALE

**Strengths:**

- Familiar patterns for anyone coming from Python LangChain
- Agent + chain abstractions

**Weaknesses:**

- Last updated October 2024 (16 months stale)
- Python-port design patterns (chains, prompt templates) are not idiomatic Rust
- Heavy dependency tree

**Fit assessment:** Abandoned. Skip.

#### claude-agent-sdk-rs -- INTERESTING BUT NICHE

**Strengths:**

- Bidirectional streaming
- Hooks and custom tools
- Claims 100% feature parity with Python Claude SDK

**Weaknesses:**

- Claude Code-specific, not general purpose
- Very new (0.6.x), 4K total downloads
- Single-provider (Anthropic/Claude)

**Fit assessment:** Worth monitoring as reference for Claude Code integration patterns, but not adoptable.

## Ion's Current Provider Layer

**Size:** ~8,900 lines across 32 files

| Component                      | Files   | Purpose                                                 | External crate could replace?         |
| ------------------------------ | ------- | ------------------------------------------------------- | ------------------------------------- |
| `anthropic/`                   | 5 files | Native Anthropic Messages API (cache_control, thinking) | Partially (no cache_control in genai) |
| `openai_compat/`               | 7 files | OpenAI/Groq/Kimi/OpenRouter/Ollama                      | Yes (genai covers most)               |
| `http/`                        | 3 files | Shared HTTP + SSE utilities                             | Yes (genai has internal SSE)          |
| `subscription/`                | 3 files | ChatGPT/Gemini OAuth flows                              | No (no crate supports this)           |
| `registry/`                    | 4 files | Model registry + filtering                              | No (Ion-specific)                     |
| `types.rs`                     | 1 file  | Canonical message types                                 | Must keep (Ion's data model)          |
| `client.rs`, `api_provider.rs` | 2 files | Provider dispatch                                       | Must keep (Ion-specific routing)      |
| `prefs.rs`, `error.rs`         | 2 files | Preferences, errors                                     | Must keep                             |
| `models_dev.rs`                | 1 file  | Dev model definitions                                   | Must keep                             |

**Estimated replaceable surface:** ~40-50% of provider code (OpenAI-compat request building, SSE parsing, basic streaming) could delegate to genai for API-key providers. The Anthropic-specific features (cache_control, fine-grained thinking config), subscription/OAuth providers, provider quirks, model registry, and canonical types must remain custom.

## Key Question: Custom vs Crate

**Verdict: Custom provider code is justified, with genai as an optional adapter for standard providers.**

### Why custom remains necessary

1. **Anthropic cache_control**: No external crate exposes this. Ion uses it for prompt caching, which materially affects cost and latency.
2. **OAuth/subscription providers**: ChatGPT and Gemini subscription auth flows are unique to Ion. No crate supports these.
3. **Provider quirks**: `max_tokens` vs `max_completion_tokens`, `store` field compat, `developer` vs `system` role, `reasoning_content` extraction -- these are Ion-tested and battle-hardened.
4. **Streaming granularity**: Ion's `StreamEvent` enum gives the TUI direct control over rendering text deltas, thinking deltas, and tool calls independently. Crate abstractions add a mapping layer.
5. **Canonical types**: Ion's `ContentBlock`, `Message`, `ChatRequest` types are tightly integrated with session persistence, tool orchestration, and TUI rendering.

### Where genai helps

1. **New provider onboarding**: Adding Together, Fireworks, Nebius, Cohere, xAI without writing protocol code.
2. **SSE parsing maintenance**: genai's unified `EventSourceStream`/`WebStream` engine removes a class of bugs.
3. **Reduced OpenAI-compat boilerplate**: 5 providers sharing one protocol through genai vs Ion maintaining its own.

### Recommended approach (unchanged from runtime integration plan)

1. **Phase 3 (genai adapter)**: Feature-gated `genai` backend for API-key providers that map cleanly.
2. **Keep custom paths** for: Anthropic (cache_control), subscription providers (OAuth), any provider with quirks not handled by genai.
3. **Adapter boundary**: Ion canonical types at all edges. `genai` is an implementation detail behind `LlmApi` trait.
4. **No agent framework adoption**: Ion's agent loop, tool orchestration, and session persistence are well-suited to its needs. rig/swarms/langchain add framework weight without solving Ion's actual problems.

## Crates Worth Monitoring

| Crate                   | Why                                                                            | Check back |
| ----------------------- | ------------------------------------------------------------------------------ | ---------- |
| **genai**               | Primary adapter candidate; watch for cache_control support, OAuth hooks        | Monthly    |
| **rig-core**            | Design patterns for Tool trait, streaming; potential MCP integration reference | Quarterly  |
| **claude-agent-sdk-rs** | Claude Code integration patterns                                               | Quarterly  |
| **async-openai**        | If Ion ever needs deep OpenAI-specific features (Responses API, Realtime)      | As needed  |
