# ion System Design

## Overview

`ion` is a high-performance Rust-based terminal agent designed for reliable task execution and clean terminal UX. The focus is a solid core agent loop, responsive TUI, and predictable tool behavior. Advanced memory systems are intentionally out of scope until the core experience is stable.

**Runtime**: Rust (Stable)
**Distribution**: Single static binary

```
User Request → ion CLI → Agent Core → Tool Execution → Response
```

## Architecture Layers

### 1. TUI Layer (`ratatui`)

Inline viewport TUI with native terminal scrollback, minimal chrome, and bottom-anchored selectors.

- **History**: Terminal scrollback (not app-managed).
- **Statusline**: `{Model} · {Context}%` left, `? help` right (no git/cwd).
- **Input**: Multi-line editor with history recall and word navigation.

### 2. Provider Layer

Multi-provider abstraction via `llm-connector` supporting:

- **Anthropic**: Direct Claude API
- **Google**: Gemini via AI Studio
- **Groq**: Fast inference
- **Ollama**: Local models
- **OpenAI**: Direct GPT API
- **OpenRouter**: 200+ models aggregator

### 3. Agent Layer

The core multi-turn loop is designed for high performance and observability:

- **Decomposed Phases**: Turn logic is split into response streaming and tool execution phases.
- **Tool Execution**: Tool calls are executed sequentially or concurrently as needed.
- **Shared State**: Core state is wrapped in `Arc` where needed to avoid expensive cloning.

## Memory and Extensions

Long-term goals include RLM integration, richer agent context management, and a memory system. These remain optional integrations via hooks or plugins, not core dependencies.

## TUI Layer (High-Performance Interaction)

- **Grapheme-Based Input**: Uses `unicode-segmentation` for robust cursor movement and editing of wide characters (CJK, Emoji).
- **Markdown Caching**: Formatted segments are cached in `MessageEntry` to ensure 60fps rendering without re-allocation.
- **Async Communication**: `tokio::sync::mpsc` channels decouple the agent's logic from the UI frame loop.

## Data Persistence

```
~/.ion/
├── config.toml          # Global settings
└── data/
    └── sessions.db      # Persisted message history (SQLite)
```

Memory is deferred and expected to arrive via hooks/plugins later, not as a core on-disk store.

### Schema Notes (Current + Future)

**Current tables (SQLite):**

- `sessions`: session metadata (id, working_dir, model, timestamps).
- `messages`: per-session transcript (`role`, JSON `content`, `position`).
- `input_history`: global input recall (UI only).

**Compatibility goals:**

- Treat `messages` as immutable transcript of record.
- Avoid in-place edits for memory/RLM features; use overlays instead.
- Use `messages.id` or `position` as stable references (if we need stronger IDs later, add an explicit `message_id` column rather than rewriting history).

**Planned add-on tables (optional):**

- `session_context`: curated context blocks with revision history.
- `memory_items`: embeddings + metadata, scoped by project/user.
- `session_settings`: RLM/policy state per session.

These should remain optional and isolated from the core transcript to keep the core agent predictable and to support a “memory off” mode.

## Tool Framework

- **Built-in**: `read`, `write`, `grep`, `glob`, `bash`, `list`, `edit`.
- **Formatting**: Tool execution is logged minimally; file edits show standard git-style diffs.
- **Safety**: 3-mode permission matrix (Read, Write, AGI) with interactive `y/n/a/A/s` prompts in Write mode.

## Agent Loop Decomposition

To improve reliability and fix "silent hang" bugs, the Agent loop is being refactored into discrete phases:

1. **Response Phase**: Handles provider streaming, collects deltas, and extracts tool calls.
2. **Tool Phase**: Executes tool calls and returns results.
3. **State Phase**: Commits assistant and tool turns to history.

This separation allows for robust error handling at each boundary and enables unit testing of tool execution without requiring a live LLM.

## TUI: Message Rendering

Message history is rendered into terminal scrollback via inline viewport inserts. The TUI maintains only the active viewport content and ephemeral selector UI state.

## Design Philosophy

- **Minimalist**: Focus on the code and the chat; avoid UI clutter.
- **Native**: Leverage Rust's speed for instant tool feedback and search.
- **Extensible**: Hooks and plugins enable optional memory systems later without bloating core UX.
