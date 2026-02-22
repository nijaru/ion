# Provider Crates Fresh Evaluation (2026-02-22)

**Purpose**: Fresh source-code verification of Rust LLM provider crates for potential use in ion.
**Supersedes**: `provider-crates-2026-02.md` (2026-02-11) and `genai-crate-deep-dive-2026-02.md` (2026-02-14)
**Method**: Direct source code inspection via GitHub + docs.rs + commit history. Not cached summaries.

**Critical requirements (disqualifying if missing):**

1. Streaming via SSE (text deltas)
2. Tool/function calling with streaming — chunks arrive incrementally, not just at end
3. Text + tool calls in SAME response simultaneously
4. Multi-provider: Anthropic, OpenAI, Google/Gemini, Groq, Ollama, DeepSeek, xAI

---

## Summary Table

| Crate        | Version (crates.io)  | Last commit | Text+Tools  | Tool stream mode | cache_control | Verdict                          |
| ------------ | -------------------- | ----------- | ----------- | ---------------- | ------------- | -------------------------------- |
| **llm**      | 1.3.7 (2026-01-09)   | 2026-02-20  | YES         | Incremental      | Tools only\*  | Best API fit; unreleased changes |
| **genai**    | 0.5.3 / 0.6.0-beta.1 | 2026-02-15  | PARTIAL\*\* | Complete only    | No            | Still disqualified               |
| **rig-core** | 0.30.0               | 2026-02-11  | YES         | Collected/end    | Yes           | Too much framework               |

\*cache_control PR merged 2026-02-20, not yet in crates.io release
\*\*Text+tools coexist in stream as events but ToolChunk = complete call, not delta; issue #60 still open

---

## 1. llm (graniet/llm) — v1.3.7

**GitHub**: https://github.com/graniet/llm
**Stars**: 310 | **Contributors**: 20
**crates.io version**: 1.3.7 (released 2026-01-09)
**Last commit**: 2026-02-20 (PR #110: cache_control for tools — NOT yet released)
**Edition**: 2021
**Confidence**: HIGH — source code directly verified

### 1a. Streaming (SSE text deltas)

**PASSES.** Two streaming modes:

- `chat_stream()` -> `Stream<Item = Result<String>>` (text only)
- `chat_stream_with_tools()` -> `Stream<Item = Result<StreamChunk>>` (structured, use this)

Both are async streams using `futures::Stream`.

### 1b. Tool calls in streaming — incremental?

**PASSES — fully incremental.** The `StreamChunk` enum:

```rust
pub enum StreamChunk {
    Text(String),
    ToolUseStart { index: usize, id: String, name: String },
    ToolUseInputDelta { index: usize, partial_json: String },
    ToolUseComplete { index: usize, tool_call: ToolCall },
    Done { stop_reason: String },
}
```

For Anthropic: SSE events map directly — `content_block_start` (tool_use type) emits `ToolUseStart`, each `input_json_delta` event emits `ToolUseInputDelta`, `content_block_stop` emits `ToolUseComplete`.

For OpenAI-compat: `parse_openai_sse_chunk_with_tools` emits `ToolUseStart` when function name arrives, `ToolUseInputDelta` for each arguments fragment, `ToolUseComplete` on `finish_reason`. Also fully incremental.

### 1c. Text + tool calls in same response

**PASSES — verified with test.** The function `parse_anthropic_sse_chunk_with_tools` processes all content block types independently. A test named `test_parse_stream_mixed_text_and_tool` explicitly validates that `StreamChunk::Text` chunks and `StreamChunk::ToolUseStart` chunks appear in the same stream for a mixed response. No filtering suppresses text when tools are present.

The `ChatResponse` trait:

```rust
pub trait ChatResponse: Debug + Display + Send + Sync {
    fn text(&self) -> Option<String>;
    fn tool_calls(&self) -> Option<Vec<ToolCall>>;
    fn thinking(&self) -> Option<String> { None }
    fn usage(&self) -> Option<Usage> { None }
}
```

Both `text()` and `tool_calls()` are independent `Option` returns — no mutual exclusion.

### 1d. Provider coverage

Feature-flagged. 15 providers:
OpenAI, Anthropic, Ollama, DeepSeek, xAI, Phind, Groq, Google, AzureOpenAI, Cohere, Mistral, OpenRouter, HuggingFace, AWS Bedrock, ElevenLabs (TTS)

Covers ion's full requirement: Anthropic, OpenAI (compat), Google/Gemini, Groq, Ollama, DeepSeek, xAI. Also has OpenRouter natively.

### 1e. cache_control (Anthropic prompt caching)

**PARTIAL — tools only, unreleased.** PR #110 merged 2026-02-20 adds `cache_control` to the `AnthropicTool` struct:

```rust
struct AnthropicTool<'a> {
    name: &'a str,
    description: &'a str,
    #[serde(rename = "input_schema")]
    schema: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<&'a serde_json::Value>,
}
```

System prompt cache_control: NOT implemented in v1.3.7 or main. The `SystemPrompt` and `RequestSystemPrompt` enums have no cache_control field. The previous research note about `SystemContent::text_with_cache()` appears to have been incorrect or from a branch — the main branch source does not show this for system prompts.

**Confidence on cache_control**: MEDIUM — based on GitHub source inspection; may differ from unreleased commits.

### 1f. Thinking / reasoning blocks

YES. `AnthropicConfig.reasoning: bool` + `thinking_budget_tokens: Option<u32>`. Sends `ThinkingConfig` in requests. `thinking()` method on `ChatResponse` trait. `ReasoningEffort` enum for OpenAI/DeepSeek.

### 1g. Custom base URL

YES. `LLMBuilder::base_url()`. Explicit `OpenRouter` backend variant. `extra_body` for arbitrary JSON.

### 1h. Feature flags

YES. Each provider is a Cargo feature. `full` enables everything. Compile only what you need.

### 1i. Architecture concerns for ion

- **Edition 2021**, not 2024
- `Box<dyn ChatResponse>` dynamic dispatch — trait objects, not static dispatch
- `reqwest ^0.12.12` (genai uses 0.13)
- CLI features (`cli`, `api`) pull in ratatui, crossterm, axum — but all behind feature flags, so no impact
- Name collision with archived `rustformers/llm` — cosmetic issue
- **Gap between crates.io (v1.3.7, 2026-01-09) and main (2026-02-20)** — PR #110 (tool cache_control) is on main but unreleased. Need to decide: use crates.io release or git dependency

### 1j. Overall verdict

**BEST FUNCTIONAL FIT for ion's requirements.** Passes all four critical requirements definitively, including incremental tool call streaming and text+tool coexistence (with test coverage). The StreamChunk enum maps nearly 1:1 to ion's StreamEvent model. The main limitation is system prompt cache_control — which ion uses heavily — is not implemented.

**Ion-specific adoption cost:**

- Cannot replace ion's Anthropic provider if system prompt cache_control is required (likely is)
- Would still need custom Anthropic path for cache_control, OAuth providers
- Estimated replaceable surface: ~40% of provider code (OpenAI-compat providers, non-Anthropic)

---

## 2. genai (jeremychone/rust-genai) — v0.5.3 / v0.6.0-beta.1

**GitHub**: https://github.com/jeremychone/rust-genai
**Stars**: 652 | **Forks**: 141
**crates.io stable**: 0.5.3 (2026-01-31) — recommended
**crates.io pre-release**: 0.6.0-beta.1 (2026-02-15) — active development
**Downloads**: 120k total / 32k per 90 days
**Edition**: 2021 (likely; not confirmed in this session)
**Confidence**: HIGH for streaming model; MEDIUM-HIGH for text+tools (source inspected via proxy)

### 2a. Streaming (SSE text deltas)

**PASSES.** `exec_chat_stream()` returns `ChatStreamResponse` containing a `ChatStream` which implements `futures::Stream<Item = Result<ChatStreamEvent>>`:

```rust
pub enum ChatStreamEvent {
    Start,
    Chunk(StreamChunk),            // text content delta
    ReasoningChunk(StreamChunk),   // reasoning/thinking delta
    ThoughtSignatureChunk(StreamChunk),
    ToolCallChunk(ToolChunk),      // tool call (complete, not delta)
    End(StreamEnd),
}
```

Unified `EventSourceStream`/`WebStream` engine across all providers since v0.5.0.

### 2b. Tool calls in streaming — incremental?

**PARTIAL — NOT incremental.** `ToolChunk` is defined as:

```rust
pub struct ToolChunk {
    pub tool_call: ToolCall,  // complete assembled tool call
}
```

In the Anthropic streamer, `ToolCallChunk(tc)` is emitted at `content_block_stop` — only when the tool call is fully assembled. The tool input JSON is accumulated internally across `input_json_delta` events but the `ToolCallChunk` event is emitted once, complete.

Implication: A consumer cannot show "tool call building..." progress. The tool call only arrives as a complete object. For ion's use case (streaming tool call arguments to TUI), this is less granular than the llm crate but may still be acceptable if ion only needs to know when a tool call is complete.

**Confidence**: HIGH — Anthropic streamer source inspected; `ToolChunk` struct verified on docs.rs.

### 2c. Text + tool calls in same response

**CONDITIONALLY PASSES — but issue #60 is still open.**

Source inspection reveals that `MessageContent` is now `Vec<ContentPart>` where `ContentPart` includes `Text`, `Binary`, `ToolCall`, and `ToolResponse` variants. The Anthropic streamer emits both `InterStreamEvent::Chunk(text)` and `InterStreamEvent::ToolCallChunk(tc)` within the same stream response.

The `ChatResponse` struct has:

```rust
pub struct ChatResponse {
    pub content: MessageContent,  // Vec<ContentPart> — can hold text + tool calls
    // ...
}
```

With methods `first_text()`, `texts()`, `tool_calls()`, `into_tool_calls()` all operating on the same heterogeneous `MessageContent`.

**HOWEVER**: Issue #60 ("text and tool calls together") remains open as of this session (last comment June 2025). The maintainer outlined a plan but no PR was merged. The `MessageContent` refactor in v0.4.0-0.5.0 may have partially addressed this — but the issue being open suggests there are still gaps in handling or the API is not ergonomic enough.

**Risk**: The issue was reported because the caller could not get text content when tools were also present. If `ChatResponse.content` is now `Vec<ContentPart>` and both types are included, the fix may be structural but the ergonomics are still being refined.

**Confidence**: MEDIUM — source shows the types can coexist; issue open suggests real-world behavior may still have problems in some providers.

### 2d. Provider coverage

14 native providers:
OpenAI, Anthropic, Gemini, xAI, Ollama, Groq, DeepSeek, Cohere, Together, Fireworks, Nebius, Mimo, Zai (Zhipu AI), BigModel

Also: any OpenAI-compat via `ServiceTargetResolver`.
Missing from ion's list: No native OpenRouter (use resolver), no Kimi (use resolver).

### 2e. cache_control (Anthropic prompt caching)

**NOT SUPPORTED.** The `ChatOptions` and `ChatRequest` types have no cache_control field. No evidence of `cache_control` in the genai API surface. The `MessageOptions` struct provides `CacheControl` for per-message cache hints — but this is a recent addition and may be in 0.6.x only.

**Update**: docs.rs shows `CacheControl` in the `chat` module:

```
CacheControl — Cache control
MessageOptions — Per-message options (e.g., cache control)
```

This may be new in 0.6.0-beta.1. If `MessageOptions` supports `CacheControl` per-message, Anthropic cache_control may be supported at the message level. Needs verification in 0.6.x source.

**Confidence on cache_control**: LOW-MEDIUM — type exists in docs but usage unclear; 0.6.x is pre-release.

### 2f. Thinking / reasoning blocks

YES. `ReasoningEffort` for both Anthropic and Gemini. `ReasoningChunk(StreamChunk)` in stream events. `reasoning_content` capture in `ChatOptions`. No explicit `budget_tokens` — coarser than ion's `ThinkingConfig.budget_tokens`.

### 2g. Custom base URL

YES. `ServiceTargetResolver` provides full control over endpoint URL per model/request.

### 2h. Feature flags

NO. All providers compiled in by default. No per-provider feature gates.

### 2i. Architecture notes

- `reqwest ^0.13` (newer than llm's 0.12)
- 84% documentation coverage (very good)
- `eventsource-stream` for SSE
- `futures::Stream` pull model (vs ion's mpsc push model — adapter needed)
- Pre-1.0 with README warning to pin exact versions; patches break APIs
- 0.6.0-beta.1 active on 2026-02-15

### 2j. Overall verdict

**DISQUALIFIED for ion's current use (same conclusion as prior research).** The blocking issues:

1. **Issue #60 still open**: Text+tool calls may work in the type system but real-world behavior is unconfirmed for all providers. Open issue with no merged PR means ion cannot rely on it.
2. **No confirmed cache_control**: ion uses Anthropic cache_control heavily; genai 0.5.3 doesn't support it. 0.6.x may add it but is pre-release.
3. **ToolCallChunk is complete, not incremental**: Less granular than ion's current StreamEvent model (less of a blocker but worth noting).
4. **Pre-1.0 instability**: API breaks in patches.

**Watch**: 0.6.0 release. If issue #60 is resolved and `MessageOptions::CacheControl` is confirmed working with Anthropic in stable release, reassess.

---

## 3. New Crates Since February 2026

### 3a. litellm-rust (v0.1.1, 2026-02-07)

**GitHub**: https://github.com/avivsinai/litellm-rust
**Downloads**: 24 (new)

Port of Python LiteLLM. Providers: OpenAI-compat, Anthropic, Gemini, xAI. Streaming yes. Tool calls: no streaming tool calls confirmed. No cache_control. MSRV 1.88.

**Verdict**: Too new, no adoption, Gemini streaming not confirmed. Skip.

### 3b. ratatoskr (v0.x, 2026-02-22)

**GitHub**: https://github.com/emesal/ratatoskr
**Stars**: 0 | **Contributors**: 1

Unified gateway: OpenRouter, Anthropic, OpenAI, Google, Ollama. Streaming + tool calling claimed. Extended thinking claimed. Cache_control: not mentioned.

**Verdict**: 0 stars, solo project, no releases tracked. Skip.

### 3c. adk-rust (2026-02-16)

**GitHub**: https://github.com/zavora-ai/adk-rust

Agent Development Kit inspired by Google's ADK. Rust agent framework with model support.

**Verdict**: Agent framework, not a provider client library. Not relevant for ion's provider layer.

### 3d. No significant new entrants

No new crates between 2026-02-14 (prior research) and 2026-02-22 pass the critical requirements bar. The field is stable: `llm` and `genai` remain the two primary candidates.

---

## 4. Status of Prior Research Claims

### "llm passes all requirements including cache_control via SystemContent::text_with_cache()"

**PARTIALLY INCORRECT.** The prior research (2026-02-14) claimed cache_control support for system prompts via `SystemContent::text_with_cache()`. Current source inspection shows:

- Tool-level cache_control: Added via PR #110 on 2026-02-20 (unreleased)
- System prompt cache_control: Not found in current main branch source

The `SystemContent::text_with_cache()` claim cannot be verified from current source. This may have been inferred from documentation or an example rather than actual source. Ion cannot rely on system prompt cache_control in `llm` v1.3.7 or current main.

**Updated confidence**: System prompt cache_control is NOT available in `llm`. Tool cache_control is present in main but unreleased.

### "genai text+tool coexistence was UNKNOWN (prior research)"

**UPDATED**: The type system now supports it (`MessageContent` is `Vec<ContentPart>`). The Anthropic streamer emits both text and tool events in the same stream. However, issue #60 remains open — the ergonomic API is not finalized and there may be provider-specific gaps.

### "llm StreamChunk has ToolUseStart/ToolUseInputDelta/ToolUseComplete"

**CONFIRMED.** Source verified. Test `test_parse_stream_mixed_text_and_tool` confirmed.

---

## 5. Definitive Feature Matrix

| Feature                       | llm v1.3.7        | genai v0.5.3         | ion custom (current)  |
| ----------------------------- | ----------------- | -------------------- | --------------------- |
| SSE text streaming            | YES               | YES                  | YES                   |
| Tool calls incremental stream | YES (3-phase)     | NO (complete only)   | YES                   |
| Text + tools same response    | YES (test-proven) | PARTIAL (issue #60)  | YES                   |
| Anthropic                     | YES               | YES                  | YES                   |
| OpenAI-compat                 | YES               | YES                  | YES                   |
| Google/Gemini                 | YES               | YES                  | YES                   |
| Groq                          | YES               | YES                  | YES                   |
| Ollama                        | YES               | YES                  | YES                   |
| DeepSeek                      | YES               | YES                  | YES                   |
| xAI                           | YES               | YES                  | YES                   |
| OpenRouter (native)           | YES               | NO (resolver only)   | YES                   |
| cache_control (system prompt) | NO                | NO (0.6.x maybe)     | YES                   |
| cache_control (tools)         | YES (unreleased)  | NO (0.6.x maybe)     | YES                   |
| Thinking/budget_tokens        | YES               | NO (ReasoningEffort) | YES                   |
| Custom base URL               | YES               | YES (resolver)       | YES                   |
| Feature flags (per-provider)  | YES               | NO                   | YES                   |
| OAuth providers               | NO                | NO                   | YES (ChatGPT, Gemini) |
| reqwest version               | 0.12              | 0.13                 | 0.12                  |
| Edition                       | 2021              | 2021                 | 2024 (ion standard)   |
| Stability                     | Pre-1.0           | Pre-1.0 (breaks)     | Internal, stable      |

---

## 6. Recommendations for ion

### Primary recommendation: Keep custom provider code

The conclusion is unchanged from prior research. **Neither crate satisfies ion's full requirements without extensive workarounds.**

Critical gaps that remain unresolved:

1. **System prompt cache_control** — ion uses this for every agent session (system prompt caching). Neither `llm` nor `genai` implement it for system prompts.
2. **OAuth providers** — ChatGPT subscription (Responses API + OAuth) and Gemini OAuth are ion-specific. No crate supports this.
3. **budget_tokens** — ion passes explicit thinking token budgets to Anthropic. `llm` has it; `genai` only has coarse `ReasoningEffort`.
4. **Incremental tool streaming** — `genai` fails this; `llm` passes.

### If adopting an external crate for non-Anthropic providers

**`llm` remains the better fit** for any partial adoption:

- All four critical requirements pass (confirmed by source + test)
- `StreamChunk` maps to ion's `StreamEvent` almost 1:1
- Feature flags mean compile only what you need
- Caveats: system prompt cache_control missing, Edition 2021, gap between main and crates.io

**Scenario**: Use `llm` for OpenAI/Groq/DeepSeek/xAI/Ollama/OpenRouter providers. Keep custom Anthropic and OAuth providers. Estimated: removes ~4k lines of provider code, adds ~200-line adapter.

**Blocking concern**: The gap between crates.io (v1.3.7, 2026-01-09) and main (2026-02-20 with tool cache_control) means choosing between a stale crates.io release or a git dependency. Neither is ideal for a production codebase.

### Watch list (next check: 2026-03-22)

| Crate   | Watch for                                                              |
| ------- | ---------------------------------------------------------------------- |
| `llm`   | v1.4.0 release with PR #110 (tool cache_control); system prompt cache  |
| `genai` | v0.6.0 stable; issue #60 resolution; confirmed Anthropic cache_control |

---

## Sources

All source inspection performed 2026-02-22.

- https://github.com/graniet/llm/blob/main/src/chat/stream.rs — StreamChunk enum (direct source)
- https://github.com/graniet/llm/blob/main/src/backends/anthropic.rs — Anthropic streaming + cache_control
- https://github.com/graniet/llm/blob/main/src/backends/openai.rs — OpenAI streaming tool calls
- https://github.com/graniet/llm/commits/main — commit history, PR #110
- https://github.com/graniet/llm/blob/main/Cargo.toml — version 1.3.7
- https://crates.io/crates/llm — 57k total, 10k/90d, updated 2026-01-09
- https://github.com/jeremychone/rust-genai/blob/main/src/chat/chat_stream.rs — ChatStreamEvent, ToolChunk
- https://github.com/jeremychone/rust-genai/blob/main/src/chat/message_content.rs — MessageContent as Vec<ContentPart>
- https://github.com/jeremychone/rust-genai/blob/main/src/adapter/inter_stream.rs — InterStreamEvent
- https://github.com/jeremychone/rust-genai/blob/main/src/adapter/adapters/anthropic/streamer.rs — Anthropic streamer
- https://github.com/jeremychone/rust-genai/issues/60 — text+tools issue (still open)
- https://docs.rs/genai/latest/genai/chat/ — MessageOptions, CacheControl types
- https://crates.io/crates/genai — 120k total, 32k/90d, 0.6.0-beta.1 on 2026-02-15
