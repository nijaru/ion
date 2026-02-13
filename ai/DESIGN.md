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

### TUI Layer (RNK-first rendering + crossterm control)

RNK is now the primary text/style renderer for TUI surfaces. crossterm remains responsible for
terminal control (raw mode, cursor movement, clear/scroll, event polling).

- **Chat history**: Append-only transcript in native terminal scrollback.
- **Bottom UI**: Ephemeral UI plane (progress/input/status) rendered each frame near terminal bottom.
- **Input**: Custom composer with rope-backed multiline editing.
- **Markdown**: pulldown-cmark + custom wrap/indent handling before terminal writes.

Current resize/reflow contract:

- Reflow repaints from canonical entries at current width (viewport-safe, no full transcript replay).
- Streaming carryover tracks committed lines by width to avoid duplicate appends after resize.
- Header content is static (version + cwd); dynamic location (`cwd [branch]`) is shown in status line.

Primary architecture target is documented in `ai/design/tui-v3-architecture-2026-02.md`.

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
