# Pi-Mono Provider Architecture Analysis

**Research Date**: 2026-02-14
**Purpose**: Detailed analysis of pi-mono's AI/provider layer for ion comparison
**Source**: https://github.com/badlogic/pi-mono/tree/main/packages/ai

---

## Architecture Overview

Pi-mono's `@mariozechner/pi-ai` is a standalone TypeScript package that provides a unified LLM API across 20+ providers. It separates cleanly from the agent and TUI layers.

### File Structure

```
packages/ai/src/
  types.ts                  # All shared types (messages, events, models, options)
  api-registry.ts           # Provider registry (Map-based, register/lookup)
  stream.ts                 # Top-level stream() and complete() entry points
  models.ts                 # Model registry, cost calculation
  models.generated.ts       # Auto-generated model catalog
  env-api-keys.ts           # Env var key detection
  providers/
    register-builtins.ts    # Auto-registers all providers on import
    simple-options.ts       # SimpleStreamOptions -> StreamOptions conversion
    transform-messages.ts   # Cross-provider message normalization
    anthropic.ts            # Anthropic Messages API
    openai-completions.ts   # OpenAI Chat Completions API
    openai-responses.ts     # OpenAI Responses API
    openai-responses-shared.ts  # Shared between responses variants
    azure-openai-responses.ts   # Azure variant
    openai-codex-responses.ts   # Codex variant
    google.ts               # Google Generative AI
    google-shared.ts        # Shared Google utilities
    google-vertex.ts        # Vertex AI variant
    google-gemini-cli.ts    # Gemini CLI variant
    amazon-bedrock.ts       # AWS Bedrock
    github-copilot-headers.ts   # Copilot auth headers
  utils/
    event-stream.ts         # EventStream<T,R> base class
    json-parse.ts           # Streaming JSON parser (partial-json)
    overflow.ts             # Token overflow handling
    http-proxy.ts           # Proxy support
    validation.ts           # Input validation
    typebox-helpers.ts      # Schema conversion
    sanitize-unicode.ts     # Unicode cleaning
    oauth/                  # OAuth flows
```

---

## Key Abstractions

### 1. Provider Registry (api-registry.ts)

Map-based registry where providers register two functions:

```typescript
interface ApiProvider {
  api: Api;
  stream(model, context, options): AssistantMessageEventStream;
  streamSimple(model, context, options): AssistantMessageEventStream;
}
```

- `stream()` = full options (temperature, headers, callbacks)
- `streamSimple()` = convenience layer that resolves thinking budgets, reasoning levels

Providers self-register on module import via `register-builtins.ts`. Registration wraps functions with API type validation.

**Comparison to ion**: Ion uses an enum-based `Backend` dispatch in `client.rs` instead of a registry. Pi-mono's registry is more extensible (extensions can register custom providers at runtime), but ion's compile-time dispatch is simpler and type-safe without runtime overhead.

### 2. Unified Message Types (types.ts)

Three message types, one content enum:

| Type                | Contents                                                         |
| ------------------- | ---------------------------------------------------------------- |
| `UserMessage`       | text or content array (text, image)                              |
| `AssistantMessage`  | text, thinking, tool calls + metadata (usage, stopReason, model) |
| `ToolResultMessage` | toolCallId, toolName, content, isError                           |

Content types: `TextContent`, `ThinkingContent`, `ImageContent`, `ToolCall`

**Key difference from ion**: Pi-mono puts provider metadata (api, provider, model, usage, stopReason) on the `AssistantMessage` itself, not as separate events. Ion keeps `Usage` as a separate `StreamEvent` variant.

### 3. Event Stream (utils/event-stream.ts)

Generic `EventStream<T, R>` with async iteration:

```typescript
class EventStream<T, R> {
  push(event: T): void; // Producer side
  [Symbol.asyncIterator](); // Consumer side
  result(): Promise<R>; // Final result
  end(result?: R): void; // Signal completion
}
```

14 event types in `AssistantMessageEvent`:

| Event                      | Purpose                              |
| -------------------------- | ------------------------------------ |
| `start`                    | Stream began                         |
| `text_start/delta/end`     | Text content                         |
| `thinking_start/delta/end` | Reasoning content                    |
| `toolcall_start/delta/end` | Tool call with streaming args        |
| `done`                     | Complete with final AssistantMessage |
| `error`                    | Error with Error object              |

**Comparison to ion**: Ion uses `mpsc::Sender<StreamEvent>` with 6 variants (TextDelta, ThinkingDelta, ToolCall, Usage, Done, Error). Pi-mono has finer granularity (start/delta/end for each content type), which enables more precise UI rendering. Ion emits complete ToolCallEvents; pi-mono streams tool call arguments incrementally.

### 4. Model System (models.ts + models.generated.ts)

`Model<TApi>` is generic over API type:

```typescript
interface Model<TApi extends Api> {
  id: string;
  name: string;
  api: TApi; // Which API protocol this model uses
  provider: string; // Which company/service
  baseUrl?: string;
  reasoning: boolean;
  supportsImages: boolean;
  inputCostPerMillionTokens: number;
  outputCostPerMillionTokens: number;
  contextWindow: number;
  maxTokens: number;
  compat?: OpenAICompletionsCompat; // Provider quirks
}
```

Models are auto-generated into `models.generated.ts` and loaded into a `Map<provider, Map<id, Model>>`.

**Key insight**: The `api` field on Model determines which provider implementation handles it. Multiple providers can share an API (e.g., Mistral, Groq, xAI all use `openai-completions`). This is a clean separation of "protocol" from "vendor."

---

## Streaming Architecture

### Per-Provider Pattern

Each provider implements the same pattern:

1. Create `AssistantMessageEventStream`
2. Launch async IIFE that:
   a. Converts messages to provider format (`convertMessages`)
   b. Converts tools to provider format (`convertTools`)
   c. Makes HTTP request with streaming
   d. Iterates SSE/chunks, pushes events to stream
   e. Calls `stream.end()` on completion

```typescript
function streamAnthropic(model, context, options): AssistantMessageEventStream {
  const stream = new AssistantMessageEventStream();
  (async () => {
    try {
      // Convert, request, iterate, push events
      stream.push({ type: "done", message: output });
    } catch (e) {
      stream.push({ type: "error", error: e });
    }
  })();
  return stream; // Returns immediately, events arrive async
}
```

### What's Shared vs Per-Provider

**Shared across all providers:**

- `EventStream<T,R>` base class and `AssistantMessageEventStream`
- `AssistantMessageEvent` discriminated union (14 event types)
- `transform-messages.ts`: normalizes tool call IDs, handles orphaned tool calls, strips incompatible thinking blocks
- `simple-options.ts`: resolves thinking budgets, clamps reasoning levels
- `parseStreamingJson()`: handles partial JSON from tool call argument streaming
- Message/Content/Tool type definitions

**Shared within provider families:**

- `google-shared.ts`: convertMessages, convertTools, mapStopReason for all 3 Google variants
- `openai-responses-shared.ts`: convertResponsesMessages, convertResponsesTools, processResponsesStream for OpenAI/Azure/Codex responses
- `github-copilot-headers.ts`: auth header generation

**Per-provider (not shared):**

- HTTP request construction (headers, URL, body format)
- SSE event parsing (Anthropic events differ from OpenAI chunks differ from Google chunks)
- Provider-specific compatibility quirks
- Thinking/reasoning parameter mapping
- Cache control (Anthropic-specific)

### Provider Compatibility (Quirks)

OpenAI-completions has a `detectCompat()` function that returns provider-specific overrides:

```typescript
interface OpenAICompletionsCompat {
  maxTokensFieldName?: string; // "max_tokens" vs "max_completion_tokens"
  supportsDeveloperRole?: boolean;
  supportsStore?: boolean;
  supportsReasoningEffort?: boolean;
  toolResultRequiresName?: boolean; // Mistral needs this
  requiresToolCallIdFormat?: string; // Mistral: 9 alphanumeric chars
  // ... more flags
}
```

**Comparison to ion**: Ion has `quirks.rs` in `openai_compat/` that serves a similar purpose. Pi-mono puts compat flags on the Model definition itself, with runtime detection as fallback. Ion keeps quirks as runtime detection only.

---

## Tool Call Handling

### Cross-Provider Tool Definition

Pi-mono uses TypeBox schemas (runtime-validated TypeScript types):

```typescript
interface Tool {
  name: string;
  description: string;
  parameters: TSchema; // TypeBox schema
}
```

Each provider converts to its native format:

- Anthropic: `input_schema` with JSON Schema
- OpenAI: `function.parameters` with JSON Schema
- Google: `functionDeclarations` with OpenAPI-style schema

### Tool Call ID Normalization

Critical cross-provider concern handled in `transform-messages.ts`:

- OpenAI Responses API generates 450+ char IDs with special characters
- Anthropic requires `^[a-zA-Z0-9_-]+$` max 64 chars
- Mistral requires exactly 9 alphanumeric chars
- Solution: deterministic hash function (`shortHash`) maps IDs, with a bidirectional map for the current conversation

### Orphaned Tool Call Recovery

When tool calls lack results (e.g., user interrupted), pi-mono inserts synthetic error results: `"No result provided"`. This prevents API errors from providers that require balanced call/result pairs.

**Comparison to ion**: Ion doesn't currently handle cross-provider tool call ID normalization or orphaned tool recovery. These would be relevant if/when supporting model switching mid-conversation.

---

## Thinking/Reasoning Handling

### Multi-Strategy Approach

| Provider  | Older Models                               | Latest Models                              |
| --------- | ------------------------------------------ | ------------------------------------------ |
| Anthropic | `thinking.type: "enabled"` + budget_tokens | `thinking.type: "adaptive"` + effort level |
| OpenAI    | `reasoning_effort` param                   | Same                                       |
| Google    | Token budget per model                     | Discrete levels (MINIMAL, LOW, HIGH)       |

### Thinking Level Abstraction

```typescript
type ThinkingLevel = "minimal" | "low" | "medium" | "high" | "xhigh";
```

`simple-options.ts` maps these to provider-specific parameters:

- Default token budgets: minimal=1024, low=2048, medium=8192, high=16384
- Reserves minimum 1024 tokens for actual output
- `xhigh` clamped to `high` for providers that don't support it

### Cross-Provider Thinking

When switching providers mid-conversation, thinking blocks from one provider can cause errors in another. `transform-messages.ts` handles this:

- Thinking with signatures: kept only if same provider, stripped otherwise
- Thinking without signatures: converted to plain text `[thinking]...[/thinking]`
- Errored/aborted messages: skipped entirely

---

## What's Good About This Architecture

1. **API vs Provider separation**: The `api` field on Model cleanly separates "which HTTP protocol" from "which vendor." Multiple vendors share one implementation (Groq, Mistral, xAI all use openai-completions).

2. **Shared-nothing streaming**: Each provider function is self-contained. No base class to override, no abstract methods. Just: create stream, push events, return. Simple to add new providers.

3. **Family-level sharing**: Google and OpenAI variants share conversion logic through `-shared.ts` files. Not forced into a base class -- just imported functions.

4. **Cross-provider message normalization**: `transform-messages.ts` handles the real-world mess of switching providers mid-conversation (ID normalization, orphaned tool calls, thinking block compatibility).

5. **Incremental tool call streaming**: 14 event types with start/delta/end lifecycle for each content type enables responsive UI updates.

6. **Quirks as data**: Provider compatibility expressed as a data structure (`OpenAICompletionsCompat`) rather than scattered conditionals. Model definitions carry their own quirks.

7. **Auto-generated model catalog**: Models generated from provider APIs, not manually maintained.

---

## What's Relevant for Ion

### Already Similar

| Concept           | Pi-mono                | Ion                              |
| ----------------- | ---------------------- | -------------------------------- |
| Backend dispatch  | Registry (Map)         | Enum (Backend)                   |
| Provider quirks   | Compat struct on Model | `quirks.rs` runtime detection    |
| Stream events     | EventStream + events   | mpsc + StreamEvent               |
| Tool accumulation | Per-provider inline    | ToolBuilder shared type          |
| Shared HTTP       | Per-provider clients   | http/ module (sse.rs, client.rs) |

### Worth Considering

1. **Finer stream events**: Pi-mono's `text_start/delta/end` + `toolcall_start/delta/end` pattern would enable better TUI rendering (e.g., show tool name before args arrive). Ion currently emits complete `ToolCall` events only after all args are accumulated.

2. **Cross-provider message transform**: Ion doesn't currently handle model switching mid-conversation. If added, the ID normalization and orphaned tool call patterns from pi-mono are battle-tested.

3. **API as routing key**: Pi-mono's `api` field on Model (separate from `provider`) is a cleaner abstraction than ion's current Provider enum that mixes vendor identity with protocol choice. Ion already partially does this (Google uses OpenAICompat), but it's implicit in `create_backend()` rather than explicit on the model.

4. **Model-level quirks**: Moving quirks from runtime detection to model metadata would be cleaner. The model knows its own constraints.

5. **Streaming JSON parser**: Pi-mono uses `partial-json` library. Ion's `ToolBuilder` concatenates strings and parses at the end. Streaming parse enables emitting `toolcall_delta` events with partial argument objects, useful for showing tool progress in the TUI.

### Not Relevant

- TypeBox schemas (ion uses serde_json::Value for tool params, which is fine)
- Registry pattern (ion's compile-time enum dispatch is more appropriate for Rust)
- OAuth complexity (pi-mono handles GitHub Copilot, Claude Code OAuth -- ion has its own auth module)
- `streamSimple` convenience layer (ion doesn't need this split)

---

## Architecture Comparison Summary

| Aspect                | Pi-mono                           | Ion                          | Assessment                       |
| --------------------- | --------------------------------- | ---------------------------- | -------------------------------- |
| Provider dispatch     | Runtime registry (Map)            | Compile-time enum            | Ion's approach better for Rust   |
| Type system           | TypeScript generics `Model<TApi>` | Provider enum + ModelInfo    | Similar, ion could add API type  |
| Streaming             | EventStream class, async iter     | mpsc channel                 | Both work; pi-mono more granular |
| Tool streaming        | Incremental (start/delta/end)     | Accumulated (complete event) | Pi-mono enables better UX        |
| Message conversion    | Per-provider `convertMessages`    | Per-provider `request.rs`    | Same pattern                     |
| Cross-provider compat | `transform-messages.ts`           | Not yet needed               | Add when supporting model switch |
| Quirks                | Data on Model + detection         | Runtime detection            | Pi-mono slightly cleaner         |
| Shared code           | `-shared.ts` files per family     | Shared `http/` module        | Both reasonable                  |
| Model catalog         | Auto-generated                    | JSON + dev fetch             | Similar                          |

---

## References

- Repository: https://github.com/badlogic/pi-mono
- AI Package: https://github.com/badlogic/pi-mono/tree/main/packages/ai
- Types: https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/types.ts
- Registry: https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/api-registry.ts
- Stream: https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/stream.ts
- Anthropic Provider: https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/providers/anthropic.ts
