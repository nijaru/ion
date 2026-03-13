# TUI Selectors and HTTP Client Research

**Date**: 2026-01-27
**Questions**: Q5 (Modal Selector UI), Q6 (llm-connector Replacement)

---

## Q5: Modal Selector UI Without ratatui Viewport

### Problem Statement

How to handle modal selector UI (model picker, provider picker, session picker) in a terminal chat app that uses native scrollback instead of ratatui Viewport?

### Options Evaluated

| Option | Description                       | Pros                          | Cons                                     |
| ------ | --------------------------------- | ----------------------------- | ---------------------------------------- |
| A      | Replace bottom area with selector | Simple, no alternate screen   | Loses input context, jarring transition  |
| B      | Push selector to scrollback       | Preserves history, clean      | Selector becomes permanent in scrollback |
| C      | Temporary alternate screen        | Full isolation, clean exit    | Breaks chat context, heavy-handed        |
| D      | Overlay approach                  | Modern, keeps context visible | Complex cursor/input handling            |

### How Similar Tools Handle This

**OpenCode (Go + SolidJS TUI)**

- Uses command palette (Ctrl+P) with dialog overlay
- DialogSelect component wraps fuzzy-searchable list
- Suspends keybind handling while palette open
- Slash commands (/) trigger autocomplete dropdown
- Source: `dialog-command.tsx`, `autocomplete.tsx`

**Pi (TypeScript)**

- **Overlay-based system**: `tui.showOverlay(component, options?)`
- Overlays render on top without replacing content
- Supports multiple positioning strategies (anchor, percentage, absolute)
- Focusable interface for cursor positioning via CURSOR_MARKER
- Model cycling via Ctrl+P (quick switch, no picker)
- Session picker in `/resume` command with fuzzy search

**Claude Code / Gemini CLI**

- Different UI approaches: Claude uses tree format, Gemini uses boxed actions
- Both favor inline rendering over modal dialogs
- Model switching typically via flags or config, not runtime picker

### Recommendation: Option A with Enhancements

**Approach**: Replace bottom UI temporarily with selector, but design it as a "mode switch" rather than a dialog.

**Rationale**:

1. **Matches terminal chat mental model**: Users expect the bottom area to be the interactive zone
2. **No alternate screen complexity**: Avoids context disruption
3. **Clean scrollback**: Selector never pollutes history
4. **Implementation simplicity**: Reuses existing bottom-area rendering infrastructure

**Design**:

```
Normal mode:          Selector mode:
+------------------+  +------------------+
| Chat history     |  | Chat history     |  (unchanged)
| (scrollback)     |  | (scrollback)     |
+------------------+  +------------------+
| Progress line    |  | Filter: [____]   |  <- replaces progress
| Input area       |  | > anthropic      |  <- replaces input
| Status line      |  |   google         |
+------------------+  |   openai         |
                      +------------------+
```

**Keybindings**:

- Enter selector: Ctrl+M (model), Ctrl+P (provider), Ctrl+S (session)
- Navigate: Arrow keys, j/k
- Filter: Type to filter
- Select: Enter
- Cancel: Esc (returns to normal mode)

**Why not overlays?**

- Overlays require z-order management
- Cursor positioning complexity with crossterm direct rendering
- Terminal capability variance (not all terminals handle overlays well)

---

## Q6: Replace llm-connector with Custom HTTP Client?

### Current State

ion uses `llm-connector v0.5.13` for LLM API calls. Issues encountered:

| Issue                               | Impact                      | Workaround Possible?   |
| ----------------------------------- | --------------------------- | ---------------------- |
| Kimi K2.5 `reasoning_content` field | Response content missing    | No - field not exposed |
| Anthropic `cache_control`           | Cannot reduce costs/latency | No - not supported     |
| Provider-specific headers           | Missing beta features       | Limited                |

### Provider Edge Cases Analysis

Based on research, here are the key provider-specific behaviors:

**Anthropic**

- `cache_control`: `{"type": "ephemeral", "ttl": "5m" | "1h"}`
- Extended TTL header: `anthropic-beta: extended-cache-ttl-2025-04-11`
- Response includes `cache_read_input_tokens`, `cache_creation_input_tokens`
- Thinking blocks have special caching rules

**Kimi K2 / Moonshot**

- `reasoning_content` field in `response.choices[0].message`
- Think tags: `<think>` / `</think>`
- Thinking mode toggle: `extra_body={'thinking': {'type': 'disabled'}}`
- Return mechanism: `reasoning-content` (separate field)

**DeepSeek R1**

- `reasoning_content` field (same pattern as Kimi)
- Must pass back `reasoning_content` during tool invocation or 400 error
- Tags: `<think>` / `</think>`, `<answer>` / `</answer>`
- Recommended: Force response start with `<think>\n`

**OpenAI o-series**

- Reasoning tokens in response (handled differently)
- `reasoning_effort` parameter: `low`, `medium`, `high`
- No separate `reasoning_content` field (embedded in response)

**Google Gemini**

- Different multimodal format
- Grounding with Google Search
- Safety settings structure

### llm-connector Assessment

**Maintenance Status**

- Version 0.5.13 on crates.io
- 11+ providers claimed (OpenAI, Anthropic, Google, Ollama, etc.)
- Last update: ~1 month ago (active)
- No GitHub repo link found in crates.io listing

**Capabilities**

- Streaming support via feature flag
- Tool/function calling
- Multi-modal (v0.5.0+)
- Universal output format

**Limitations**

- No `cache_control` for Anthropic
- No `reasoning_content` extraction
- No provider-specific response fields
- Limited header customization
- Fixed request/response types

### Alternative: graniet/llm Crate

**Capabilities**:

- 12+ providers: OpenAI, Anthropic, Ollama, DeepSeek, xAI, Phind, Groq, Google, Cohere, Mistral, Hugging Face, ElevenLabs
- Reasoning support: `anthropic_thinking_example`, `openai_reasoning_example`
- Vision support
- Extensible via traits (`ChatProvider`, `CompletionProvider`)
- 484 commits (active)

**Assessment**: More feature-complete but still may not expose `reasoning_content` properly.

### Custom HTTP Client Complexity

**What we need**:

1. Streaming SSE parsing (already have reqwest)
2. Provider-specific request builders
3. Provider-specific response parsers
4. Error handling per provider
5. Rate limit handling

**Estimated effort**:

| Component         | Lines of Code | Complexity                       |
| ----------------- | ------------- | -------------------------------- |
| Anthropic client  | ~300          | Medium (cache_control, thinking) |
| OpenAI-compatible | ~200          | Low (standard format)            |
| Google Gemini     | ~250          | Medium (different format)        |
| Streaming parser  | ~150          | Medium (SSE handling)            |
| Shared types      | ~100          | Low                              |
| **Total**         | ~1000         | Medium                           |

**Benefits of custom client**:

- Full control over request/response
- Direct access to all provider fields
- Can add features as providers evolve
- Simpler debugging (no abstraction layer)
- No external dependency maintenance risk

### Recommendation: Gradual Migration

**Phase 1**: Add Anthropic-native client alongside llm-connector

- Implement `cache_control` support
- Handle thinking blocks properly
- Use for Anthropic only, keep llm-connector for others

**Phase 2**: Add reasoning model support

- Implement `reasoning_content` extraction
- Support Kimi K2, DeepSeek R1, OpenAI o-series
- Unified `ThinkingContent` type across providers

**Phase 3**: Migrate remaining providers

- OpenAI (straightforward, well-documented)
- Google Gemini (different format)
- Groq, Ollama (OpenAI-compatible)
- OpenRouter (OpenAI-compatible + routing)

**Phase 4**: Remove llm-connector dependency

**Decision Matrix**:

| Factor                              | llm-connector | Custom Client |
| ----------------------------------- | ------------- | ------------- |
| Time to implement cache_control     | Blocked       | ~4 hours      |
| Time to implement reasoning_content | Blocked       | ~4 hours      |
| Maintenance burden                  | External      | Internal      |
| Feature velocity                    | Dependent     | Full control  |
| Debugging                           | Harder        | Easier        |
| Code size                           | Smaller       | +1000 LOC     |

**Verdict**: Custom client is the right choice for ion's goals. The ~1000 LOC investment pays off in:

1. Immediate access to cache_control (up to 90% cost reduction)
2. Proper reasoning model support (Kimi K2.5, DeepSeek R1)
3. Future-proofing for provider-specific features
4. Simpler debugging and maintenance

---

## Implementation Notes

### Anthropic cache_control Format

```json
{
  "model": "claude-sonnet-4-5",
  "system": [
    {
      "type": "text",
      "text": "System prompt...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [...]
}
```

Response includes:

```json
{
  "usage": {
    "input_tokens": 21,
    "cache_creation_input_tokens": 188086,
    "cache_read_input_tokens": 0,
    "output_tokens": 393
  }
}
```

### Reasoning Content Extraction

```rust
// Unified type for reasoning across providers
pub enum ThinkingContent {
    /// Anthropic extended thinking
    Anthropic { thinking: String },
    /// Kimi K2, DeepSeek R1 style
    ReasoningContent { reasoning_content: String },
    /// OpenAI o-series (embedded in response)
    Embedded { reasoning_tokens: u32 },
}

// Response parsing
if let Some(reasoning) = response.choices[0].message.get("reasoning_content") {
    // Kimi K2, DeepSeek R1
}
```

---

## Sources

### Q5 Sources

- [OpenCode TUI Documentation](https://opencode.ai/docs/tui/)
- [OpenCode GitHub](https://github.com/opencode-ai/opencode)
- [pi-mono TUI Package](https://github.com/badlogic/pi-mono/tree/main/packages/tui)
- [Ratatui Alternate Screen](https://ratatui.rs/concepts/backends/alternate-screen/)
- [Claude Code vs Gemini CLI Comparison](https://shipyard.build/blog/claude-code-vs-gemini-cli/)

### Q6 Sources

- [Anthropic Prompt Caching Documentation](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
- [llm-connector on crates.io](https://crates.io/crates/llm-connector)
- [graniet/llm on GitHub](https://github.com/graniet/llm)
- [Kimi K2.5 on OpenRouter](https://openrouter.ai/moonshotai/kimi-k2.5)
- [Kimi K2 Thinking Model](https://openrouter.ai/moonshotai/kimi-k2-thinking)
- [DeepSeek Thinking Mode Documentation](https://api-docs.deepseek.com/guides/thinking_mode)
- [LLM API Provider Comparison 2025](https://intuitionlabs.ai/articles/llm-api-pricing-comparison-2025)
