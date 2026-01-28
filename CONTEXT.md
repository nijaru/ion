# ion - Codex Context

Fast, lightweight TUI coding agent in Rust.

## Quick Start

```bash
cargo build              # Build
cargo test               # Test (113 passing)
cargo run                # Run TUI
```

## Project Structure

```
src/
├── main.rs          # TUI entry point, render loop
├── tui/             # Terminal interface (pure crossterm, no ratatui)
│   ├── mod.rs       # App state
│   ├── render.rs    # Bottom UI rendering
│   ├── events.rs    # Event handling
│   ├── session.rs   # Session initialization
│   ├── terminal.rs  # StyledLine/StyledSpan primitives
│   ├── composer/    # Custom text input (ropey-backed)
│   └── chat_renderer.rs  # Message formatting
├── agent/           # Multi-turn conversation loop
├── provider/        # LLM providers via llm crate
├── tool/            # Built-in tools + MCP client
├── skill/           # SKILL.md loader (YAML frontmatter)
└── session/         # SQLite persistence

ai/                  # Session context (read before working)
├── STATUS.md        # Current state, known issues
├── DESIGN.md        # Architecture overview
├── DECISIONS.md     # Decision log
├── design/          # Detailed designs
└── research/        # Reference material (33 files, consolidated)
```

## Current State

- **Phase:** TUI v2 Complete
- **Status:** Testing
- **Known Issues:**
  - Version line shows 3-space indent (needs investigation)
  - Kimi k2.5 API returns malformed JSON

## TUI Architecture (v2)

Pure crossterm, native terminal scrollback:

1. **Chat** → insert_before pattern (ScrollUp + print above UI)
2. **Bottom UI** → cursor positioning at `height - ui_height`
3. **Resize** → clear screen, reprint all chat
4. **Exit** → clear bottom UI only, chat stays in scrollback

Key files:

- `src/main.rs` - Render loop with insert_before pattern
- `src/tui/render.rs` - `draw_direct()`, progress/input/status rendering
- `src/tui/terminal.rs` - StyledLine/StyledSpan primitives

## Priority Tasks

From `tk ls`:

| Priority | Task    | Description                                |
| -------- | ------- | ------------------------------------------ |
| P2       | tk-268g | Anthropic caching - 50-100x cost savings   |
| P2       | tk-80az | Image attachment - @image:path syntax      |
| P2       | tk-ik05 | File autocomplete - @ triggers path picker |
| P2       | tk-hk6p | Command autocomplete - / and // prefixes   |
| P2       | tk-1lso | BUG: Kimi k2.5 API error                   |

## Code Standards

- **Toolchain:** Rust stable, Edition 2024
- **Errors:** anyhow (app), thiserror (lib)
- **Async:** tokio (network), sync (files)
- **Patterns:** `&str` over `String`, `crate::` over `super::`
- **Dependencies:** Use `cargo add`, never edit Cargo.toml versions manually

## Task Tracking

```bash
tk ls              # List all tasks
tk add "title"     # Add task
tk start <id>      # Start working
tk done <id>       # Mark complete
tk show <id>       # Get details
```

## Before Starting

1. Read `ai/STATUS.md` for current state
2. Run `tk ls` to see open tasks
3. Pick a task and run `tk start <id>`
4. When done: `tk done <id>` and commit

## Suggested First Tasks

1. **TUI Code Review** - See `TUI-REVIEW.md` for comprehensive review guide with file list, known issues, and checklist.

2. **Fix Kimi API error (tk-1lso)** - OpenRouter returns malformed JSON for Kimi k2.5. Check `src/provider/` for response parsing.

3. **Anthropic caching (tk-268g)** - Implement direct Anthropic client with cache_control. See `ai/research/prompt-caching-providers-2026.md` for context.
