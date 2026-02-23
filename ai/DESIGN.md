# ion System Design

## Overview

Fast, lightweight TUI coding agent in Rust.

**Runtime**: Rust (Stable, Edition 2024)
**Distribution**: Single static binary

```
User Request → ion CLI → Agent Core → Tool Execution → Response
```

## Module Architecture

```
src/
├── agent/          # Multi-turn agent loop
├── auth/           # OAuth + credential storage
├── compaction/     # Context summarization
├── config/         # TOML config loading
├── mcp/            # Model Context Protocol client
├── provider/       # LLM API clients
│   ├── anthropic/  # Native Anthropic Messages API
│   ├── openai_compat/  # OpenAI/OpenRouter/Groq/Kimi
│   ├── http/       # Shared HTTP + SSE utilities
│   └── ...
├── session/        # SQLite persistence
├── skill/          # YAML skill definitions
├── tool/           # Built-in + MCP tools
└── tui/            # Terminal UI
```

### Provider Layer (Native HTTP)

Three protocol implementations:

| Protocol  | Providers                                       | Features                       |
| --------- | ----------------------------------------------- | ------------------------------ |
| Anthropic | Anthropic                                       | cache_control, thinking blocks |
| OpenAI    | OpenAI, ChatGPT, OpenRouter, Groq, Kimi, Ollama | provider routing, reasoning    |
| Google    | Google, Gemini                                  | function calling               |

Provider quirks handled in `openai_compat/quirks.rs`:

- `max_tokens` vs `max_completion_tokens`
- `store` field compatibility
- `developer` vs `system` role
- `reasoning_content` extraction

### Agent Layer

Core multi-turn loop with decomposed phases:

1. **Response Phase**: Stream provider response, collect deltas, extract tool calls
2. **Tool Phase**: Execute tool calls via orchestrator
3. **State Phase**: Commit assistant and tool turns to history

### TUI Layer (crossterm-direct, no rnk)

rnk removed. All styling via `src/tui/ansi.rs` using `crossterm::style::ContentStyle`.

| Component      | Description                                                           |
| -------------- | --------------------------------------------------------------------- |
| Chat history   | Append-only transcript in native terminal scrollback (no buffer)      |
| Bottom UI      | Ephemeral rows cursor-positioned at terminal bottom each frame        |
| Input/composer | Rope-backed (`ropey`) multiline editor; blob storage for large pastes |
| Markdown       | `pulldown-cmark` + custom wrap/indent via `src/tui/text.rs`           |
| Rendering      | Direct crossterm escape sequences; `ansi::render_line`/`render_spans` |

**Current architecture** (ion-specific cleanup complete as of 2026-02-22):

- `src/tui/text.rs` — single source of truth for `display_width`, `wrap_text`, `truncate_to_width`
- `src/tui/ansi.rs` — thin ANSI builder over `crossterm::ContentStyle`
- `src/tui/render/buffer.rs` — row-string buffer with `diff()`/`to_plain_lines()` for tests
- `src/tui/composer/` — `ComposerState` takes explicit `width` param (no cached state)
- `src/tui/render/chat.rs` — split renderer functions: `render_user_message`, `render_agent_text`, etc.

**`crates/tui/` general-purpose library** (Phases 1–6 complete, 2026-02-23):

| Phase | Deliverable                                             | Status |
| ----- | ------------------------------------------------------- | ------ |
| 1     | `geometry`, `style`, `buffer`, `terminal`               | Done   |
| 2     | `event`, `app` (App trait + AppBuilder + AppRunner)     | Done   |
| 3     | `layout` (Taffy), `Text`, `Row`, `Col`, `Block`         | Done   |
| 4     | `Input` widget (multiline, keybindings, history)        | Done   |
| 5     | `List` (virtual scroll), `Scroll`                       | Done   |
| 6     | `Canvas`, `Theme`, `render_to_ansi`, inline mode polish | Done   |

Key design: Elm-style `App` trait + `Effect` system (chosen over React hooks — scales
better for LLM streaming state, more testable). Taffy flexbox layout. Cell-based
double-buffered diff. Inline + fullscreen modes. Snapshot testing built in.

**Two-layer architecture:**

- `crates/tui/` — generic library, zero ion knowledge
- `src/ui/` — agent-specific widgets built on `Canvas`: `StreamingText` ✓, `ConversationView` ✓, `ToolCallView` ✓

**Up next (ion integration):** `CodeBlock` (syntax-highlighted), `DiffView`, then wire
into ion's actual render loop to replace `src/tui/`.

**Tool display format:** Follow Claude Code's `⏺ ToolName(args)` / `⎿ result` pattern
for `ToolCallView` header and result display.

Spec: `ai/design/tui-lib-spec.md`

**Open design question:** Inline mode grow-direction. Current: grows downward from
`start_row`. For bottom-anchored input bars (ion's use case), grow-up (scroll terminal
up, keep cursor at bottom) would be more natural — requires dynamic `start_row` tracking.

### Tool Framework

- **Built-in**: `read`, `write`, `edit`, `bash`, `glob`, `grep`, `list`
- **MCP**: Client support for external tool servers
- **Permission matrix**: Read/Write modes (sandbox-based security)

## Data Model

### Message Types (provider-agnostic)

```rust
pub enum ContentBlock {
    Text { text: String },
    Thinking { thinking: String },
    ToolCall { id, name, arguments },
    ToolResult { tool_call_id, content, is_error },
    Image { media_type, data },
}

pub struct Message {
    pub role: Role,  // System, User, Assistant, ToolResult
    pub content: Arc<Vec<ContentBlock>>,
}
```

Conversion to provider format happens at request time in each client's `build_request()`.

### Persistence

```
~/.ion/
├── config.toml          # Global settings
└── data/
    └── sessions.db      # SQLite with WAL mode
```

**Tables:**

- `sessions`: metadata (id, working_dir, model, timestamps)
- `messages`: transcript (role, JSON content, position)
- `input_history`: global input recall

## Design Principles

- **Minimalist**: Focus on code and chat; avoid UI clutter
- **Native**: Leverage terminal scrollback for history
- **Simple**: Prefer clear+reprint over complex diffing
- **Provider-agnostic**: Canonical message format, convert at edges
