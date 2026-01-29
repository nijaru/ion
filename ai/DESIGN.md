# ion System Design

## Overview

`ion` is a high-performance Rust-based terminal agent designed for reliable task execution and clean terminal UX. The focus is a solid core agent loop, responsive TUI, and predictable tool behavior.

**Runtime**: Rust (Stable)
**Distribution**: Single static binary

```
User Request → ion CLI → Agent Core → Tool Execution → Response
```

## Architecture Layers

### 1. TUI Layer (crossterm)

Direct crossterm rendering with native terminal scrollback. No ratatui.

- **Chat history**: Printed to stdout, lives in terminal scrollback
- **Bottom UI**: Cursor-positioned at terminal height - ui_height
- **Input**: Custom composer with ropey-backed buffer
- **Markdown**: pulldown-cmark for rendering

Key pattern: `insert_before` - scroll up to make room, print at ui_start, then render bottom UI.

### 2. Provider Layer

Multi-provider abstraction via `llm-connector`:

- **Anthropic**: Direct Claude API
- **Google**: Gemini via AI Studio
- **Groq**: Fast inference
- **Kimi**: Anthropic-compatible Messages API (native)
- **Ollama**: Local models
- **OpenAI**: Direct GPT API
- **OpenRouter**: 200+ models aggregator

**Known limitations**: llm-connector lacks `cache_control` (Anthropic), `provider` routing (OpenRouter), and `reasoning_content` extraction (Kimi).

### 3. Agent Layer

Core multi-turn loop with decomposed phases:

1. **Response Phase**: Stream provider response, collect deltas, extract tool calls
2. **Tool Phase**: Execute tool calls via orchestrator
3. **State Phase**: Commit assistant and tool turns to history

### 4. Tool Framework

- **Built-in**: `read`, `write`, `edit`, `bash`, `glob`, `grep`, `list`
- **MCP**: Client support for external tool servers
- **Permission matrix**: Read/Write/AGI modes with interactive prompts

## Data Persistence

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

## TUI Architecture (v2)

### Rendering Model

```
Terminal
┌─────────────────────────────────────┐
│ [scrollback - terminal managed]     │ ← Chat history lives here
│ ...                                 │
│ Last message                        │
├─────────────────────────────────────┤ ← ui_start = height - ui_height
│ Progress line (when running)        │
│ ┌─────────────────────────────────┐ │
│ │ Input composer                  │ │ ← Bottom UI (cursor-positioned)
│ └─────────────────────────────────┘ │
│ Status line                         │
└─────────────────────────────────────┘
```

### Key Components

| Component          | Purpose                             |
| ------------------ | ----------------------------------- |
| `chat_renderer.rs` | Format messages → StyledLine        |
| `composer/`        | Input buffer with cursor, selection |
| `render.rs`        | Direct crossterm output             |
| `highlight.rs`     | Markdown + syntax highlighting      |
| `table.rs`         | Width-aware table rendering         |

### Resize Handling

On resize: clear screen, reprint all chat from `message_list.entries`, then render bottom UI. Simple but correct.

### Selector UI

Full-height overlay replaces bottom UI. On exit, trigger `needs_full_repaint` to restore chat.

## Design Philosophy

- **Minimalist**: Focus on code and chat; avoid UI clutter
- **Native**: Leverage terminal scrollback for history
- **Simple**: Prefer clear+reprint over complex diffing
- **Extensible**: Hooks/plugins for optional features
