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

### TUI Layer (crossterm)

Direct crossterm rendering with native terminal scrollback. No ratatui.

- **Chat history**: Printed to stdout, lives in terminal scrollback
- **Bottom UI**: Cursor-positioned at terminal height - ui_height
- **Input**: Custom composer with ropey-backed buffer
- **Markdown**: pulldown-cmark for rendering

Key pattern: `insert_before` - scroll up to make room, print at ui_start.

**Module structure (~9,300 lines excl. tests):**

| Module                                          | Lines | Purpose                               |
| ----------------------------------------------- | ----- | ------------------------------------- |
| `run.rs`, `mod.rs`, `types.rs`, `events.rs`     | 1,458 | Core loop, App struct, event handling |
| `composer/`                                     | 1,216 | Multiline input with rope buffer      |
| `render/`                                       | 1,114 | Direct crossterm rendering            |
| `message_list.rs`, `chat_renderer.rs`           | 1,332 | Message display and formatting        |
| `highlight/`                                    | 527   | Markdown + syntax highlighting        |
| `table.rs`                                      | 567   | Markdown table rendering              |
| `*_picker.rs`, `*_completer.rs`                 | 1,456 | Selection UIs (duplicated patterns)   |
| `session/`                                      | 784   | Provider setup, session management    |
| `terminal.rs`, `util.rs`, `image_attachment.rs` | 866   | Utilities (120 lines unused)          |

**Known issues (see ai/review/tui-analysis-2026-02-04.md):**

- Picker/completer code duplication (~500 lines saveable with traits)
- Unused Terminal struct (~120 lines dead code)
- 5 panic-causing bugs in composer/state.rs and visual_lines.rs
- Long functions in events.rs (420, 155 lines)
- App struct has 35+ fields (should decompose)

### Tool Framework

- **Built-in**: `read`, `write`, `edit`, `bash`, `glob`, `grep`, `list`
- **MCP**: Client support for external tool servers
- **Permission matrix**: Read/Write/AGI modes with interactive prompts

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
