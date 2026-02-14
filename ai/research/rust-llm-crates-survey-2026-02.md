# Rust LLM Provider Crates Survey (2026-02)

**Research Date**: 2026-02-14 (updated)
**Purpose**: Evaluate Rust crates for unified multi-provider LLM access with streaming and tool calling
**Showstopper Requirement**: Text + tool calls in the same response (Anthropic and OpenAI both do this)
**Requirements**: Streaming (SSE), tool/function calling, multi-provider (Anthropic, OpenAI, Google, etc.), tokio async

---

## Critical Requirement: Text + Tool Calls in Same Response

Both Anthropic and OpenAI models can return text content alongside tool calls in a single response. For example, Claude might say "I'll look that up for you" (text) and simultaneously emit a tool_use block. Any crate that models the response as either text OR tool call (but not both) is disqualified.

**ion's current approach**: `ContentBlock` enum with `Vec<ContentBlock>` per message -- handles this correctly by design.

---

## Summary Table

| Crate              | Version | Downloads (90d) | Text + Tools | Streaming | Thinking | Cache | Custom URL | Verdict                           |
| ------------------ | ------- | --------------- | ------------ | --------- | -------- | ----- | ---------- | --------------------------------- |
| **rig-core**       | 0.30.0  | 119k            | YES (v0.28+) | Yes       | Yes      | Yes   | Yes        | Fixed in PR #370, heavy framework |
| **llm**            | 1.3.7   | 10k             | YES          | Yes       | Yes      | Yes   | Yes        | Best fit for requirements         |
| **genai**          | 0.5.3   | 32k             | UNKNOWN      | Yes       | Yes      | No    | Yes        | Needs source verification         |
| **async-openai**   | 0.32.4  | 1.3M            | YES          | Yes       | No       | No    | Yes        | OpenAI-only wire format           |
| **llm-connector**  | 0.5.13  | 1k              | UNKNOWN      | Yes       | Partial  | No    | Yes        | Low adoption                      |
| **multi-llm**      | 1.0.0   | 66              | UNKNOWN      | Post-1.0  | No       | Yes   | No         | Too new, no streaming yet         |
| **langchain-rust** | 5.0.1   | ~2k             | UNKNOWN      | Yes       | No       | No    | Yes        | Port of Python LangChain          |
| **ai-lib**         | 0.4.0   | ~5k             | UNKNOWN      | Yes       | No       | No    | Yes        | Too new to evaluate               |

---

## Detailed Analysis

### 1. llm (graniet/llm) -- BEST FIT

**GitHub**: https://github.com/graniet/llm (306 stars, 68 forks, 19 contributors)
**crates.io**: https://crates.io/crates/llm (57k total, 10k/90d)
**Last commit**: 2026-02-02

#### Text + Tool Calls: YES

The `ChatResponse` trait has independent methods:

```rust
pub trait ChatResponse: Debug + Display + Send + Sync {
    fn text(&self) -> Option<String>;
    fn tool_calls(&self) -> Option<Vec<ToolCall>>;
    fn thinking(&self) -> Option<String> { None }
    fn usage(&self) -> Option<Usage> { None }
}
```

The Anthropic implementation iterates `content` blocks and filters by type -- text blocks go to `text()`, tool_use blocks go to `tool_calls()`, thinking blocks go to `thinking()`. They coexist independently.

#### Streaming: YES

Two streaming modes:

- `chat_stream()` -> `Stream<Item = Result<String>>` (text-only)
- `chat_stream_with_tools()` -> `Stream<Item = Result<StreamChunk>>` (structured)

`StreamChunk` enum:

```rust
pub enum StreamChunk {
    Text(String),
    ToolUseStart { index, id, name },
    ToolUseInputDelta { index, partial_json },
    ToolUseComplete { index, tool_call },
    Done { stop_reason },
}
```

This maps well to ion's `StreamEvent` enum.

#### Thinking/Reasoning: YES

- `AnthropicConfig.reasoning: bool` + `thinking_budget_tokens: Option<u32>`
- `ThinkingConfig` struct sent in requests
- `thinking()` method on `ChatResponse` trait
- Anthropic thinking example in repo
- `ReasoningEffort` enum (Low/Medium/High) for OpenAI/DeepSeek
- Issue #97 tracks Google Gemini thinking token support

#### Cache Control: YES

- `SystemContent::text_with_cache()` for system prompt caching
- `SystemPrompt::Messages(Vec<SystemContent>)` for structured system prompts with per-segment cache control
- Anthropic usage tracks `cache_creation_input_tokens` and `cache_read_input_tokens`

#### Custom Base URL: YES

- `LLMBuilder::base_url()` method
- Explicit `OpenRouter` backend variant
- `extra_body` field for arbitrary JSON additions to requests

#### Providers (15)

OpenAI, Anthropic, Ollama, DeepSeek, xAI, Phind, Google, Groq, AzureOpenAI, ElevenLabs, Cohere, Mistral, OpenRouter, HuggingFace, AWS Bedrock

Feature-flagged: only compile what you use.

#### Architecture

- Builder pattern for configuration
- `ChatProvider` trait (chat, chat_with_tools, chat_stream, chat_stream_with_tools)
- `CompletionProvider` trait
- `EmbeddingProvider` trait
- Feature-flagged backends
- CLI binary (optional, behind `cli` feature)
- REST API server (optional, behind `api` feature)

#### Weaknesses

- Edition 2021 (not 2024)
- CLI/API features bring heavy deps (ratatui, crossterm, axum) -- but feature-flagged
- No `pub(crate)` discipline -- leaks internal types
- `Box<dyn ChatResponse>` dynamic dispatch (trait object, not static)
- Name collision with the old `llm` crate (rustformers/llm)
- ~1084 KB repo size, moderate complexity

#### Code Mapping to ion

| ion Type         | llm Equivalent                |
| ---------------- | ----------------------------- |
| `StreamEvent`    | `StreamChunk`                 |
| `ToolCallEvent`  | `ToolCall`                    |
| `Usage`          | `Usage`                       |
| `ContentBlock`   | `AnthropicContent` (internal) |
| `ChatRequest`    | `LLMBuilder` + messages       |
| `ThinkingConfig` | `ThinkingConfig` (internal)   |

---

### 2. rig-core (0xPlaygrounds/rig)

**GitHub**: https://github.com/0xPlaygrounds/rig (5.9k stars, 653 forks)
**crates.io**: https://crates.io/crates/rig-core (234k total, 119k/90d)
**Last commit**: 2026-02-11

#### Text + Tool Calls: YES (fixed in v0.28+)

Prior to PR #370 (merged ~2025-03), `ModelChoice` was:

```rust
enum ModelChoice {
    Message(String),
    ToolCall(String, Value),
}
```

This was either/or. Bug #179 reported that only the first tool call was handled.

After PR #370, `CompletionResponse` uses:

```rust
pub struct CompletionResponse<T> {
    pub choice: OneOrMany<AssistantContent>,
    pub usage: Usage,
    pub raw_response: T,
}

pub enum AssistantContent {
    Text(Text),
    ToolCall(ToolCall),
    Reasoning(Reasoning),
    Image(Image),
}
```

`OneOrMany<AssistantContent>` supports multiple content blocks, so text + tool calls now coexist.

#### Thinking/Reasoning: YES

`AssistantContent::Reasoning` variant exists.

#### Cache Control: YES

Anthropic integration docs mention prompt caching capabilities.

#### Strengths

- Most popular/downloaded (after async-openai)
- 20+ providers, 10+ vector stores
- MCP client integration (rmcp)
- WASM support
- Multi-turn reasoning loops with `.multi_turn(n)`
- Active development

#### Weaknesses

- Heavy framework (4.18 MB source, 34k SLoC)
- 32.5% documentation coverage
- Opinionated agent abstractions conflict with custom agent loops
- 124 open issues
- Would require adapting ion's agent loop to Rig's patterns

#### Assessment

Rig is the most popular and feature-rich option. The text+tools issue is fixed. However, it is an **agent framework**, not a provider client. Using it in ion would mean either:

1. Replacing ion's agent loop with Rig's (massive rewrite, loss of control)
2. Using only the provider/model layer and ignoring the agent parts (fighting the framework)

Neither is ideal for ion's architecture.

---

### 3. genai (jeremychone/rust-genai)

**GitHub**: https://github.com/jeremychone/rust-genai (621 stars)
**crates.io**: https://crates.io/crates/genai (119k total, 32k/90d)

#### Text + Tool Calls: PREVIOUSLY REJECTED

genai was previously rejected for this exact reason. The prior survey notes it as the "best thin multi-provider client" but the user found the text+tools limitation. The response type would need source-level verification to confirm if this was fixed in v0.5.x or the v0.6.0-alpha. Given that it was the showstopper reason for rejection, and the API is still evolving, this remains risky.

#### Assessment

Would need to verify `ChatResponse` in v0.5.3+ to see if content blocks support mixed text+tool_use. The thin architecture is appealing but the known limitation may persist.

---

### 4. async-openai (64bit/async-openai)

**GitHub**: https://github.com/64bit/async-openai (1.8k stars, 342 commits, 92 contributors)
**crates.io**: https://crates.io/crates/async-openai (3M total, 1.3M/90d)

#### Text + Tool Calls: YES

Follows OpenAI's wire format where `choices[].message` has both `content` (text) and `tool_calls` (array) fields independently. They naturally coexist.

#### Limitation

OpenAI wire format only. No native Anthropic Messages API support. No Gemini API support. Would need separate crates for non-OpenAI providers.

---

### 5. Other Crates Evaluated

#### vllora_llm (new, Dec 2025)

- Reddit announcement Dec 2025
- OpenAI, Anthropic, Gemini, AWS Bedrock
- Too new to evaluate, minimal adoption data

#### saorsa-ai (v0.4.0)

- Multi-provider with reqwest + SSE
- 36 downloads/month -- negligible adoption
- thiserror v2 (modern)

#### ai-lib (v0.4.0)

- "Unified AI SDK" -- 5k total downloads
- Too new, limited documentation

#### langchain-ai-rust (v5.0.1)

- Port of Python LangChain
- 20+ providers, chains, agents, RAG
- Failed to build on docs.rs -- quality concern
- Likely too heavy and Python-flavored

#### llm-kit-provider (v0.1.2)

- 12 providers, agents, storage
- 36 downloads/month
- Too new, negligible adoption

---

## Recommendation

### For ion specifically: Keep custom provider code

The ~9000 lines in `src/provider/` are well-structured and handle:

- Text + tool calls in same response (via `ContentBlock` enum in `Vec`)
- Streaming with `StreamEvent` enum (TextDelta, ThinkingDelta, ToolCall, Usage, Done, Error)
- Thinking/extended thinking with budget_tokens
- Cache control (Anthropic)
- Provider quirks (`openai_compat/quirks.rs`)
- Model registry with capability metadata

No external crate matches this exact feature set without either:

1. Being too much framework (rig-core)
2. Missing the text+tools requirement (genai, historically)
3. Missing key providers (async-openai)
4. Being too immature (multi-llm, vllora_llm, ai-lib)

### If adopting an external crate

**First choice: `llm` (graniet/llm)** -- the only crate that:

- Passes all requirements (text+tools, streaming, thinking, cache)
- Has a clean trait-based architecture (`ChatResponse`, `ChatProvider`)
- Feature-flags providers (compile only what you need)
- Maps well to ion's existing types (`StreamChunk` ≈ `StreamEvent`)
- Does NOT impose an agent framework

**Migration cost**: Moderate. Would replace `src/provider/` (~9k lines) with llm dependency + thin adapter layer (~500 lines) mapping llm types to ion types.

**Risk**: Edition 2021, `Box<dyn ChatResponse>` dynamic dispatch, and the repo shows moderate but not heavy activity (last commit 2 weeks ago). Feature completeness is good but polish is uneven.

**Second choice: `rig-core`** -- only if willing to adopt its agent framework patterns more broadly. The text+tools issue is now fixed and it is the most actively maintained option.

---

## Sources

- https://github.com/graniet/llm (source code reviewed for ChatResponse, StreamChunk, AnthropicConfig)
- https://github.com/0xPlaygrounds/rig (PR #370, issue #179, CompletionResponse)
- https://docs.rs/rig-core/latest/rig/completion/
- https://crates.io/crates/llm
- https://crates.io/crates/rig-core
- https://crates.io/crates/async-openai
- https://crates.io/crates/genai
- https://crates.io/crates/llm-connector
- https://crates.io/crates/multi-llm
- https://crates.io/crates/ai-lib
- https://crates.io/crates/langchain-ai-rust
- https://crates.io/crates/llm-kit-provider
- https://crates.io/crates/saorsa-ai
- https://docs.rig.rs/docs/concepts/completion
- https://docs.rig.rs/docs/integrations/model_providers/anthropic
