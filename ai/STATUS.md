# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-24 |
| Status     | Runnable        | 2026-01-24 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | cargo check     | 2026-01-24 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Work

**Sprint 6: TUI Module Refactor - COMPLETE**

Split tui/mod.rs into 6 focused modules (types, util, input, events, render, session).
Fixed: Double blank line after chat messages.

**Token Counting (tk-prsa) - COMPLETE**

Fixed both issues:

1. Status line now includes system prompt in context % calculation
2. Progress line accumulates input tokens across API calls within task
3. Added `get_system_prompt()` to avoid message cloning overhead (1ba1ec8)

**Newline Input (tk-9y0p) - COMPLETE**

Added Alt+Enter and Ctrl+J as universal fallbacks for inserting newlines.
Shift+Enter only works with Kitty keyboard protocol (most terminals lack support).

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Unified provider via `llm-connector` crate (OpenRouter, Anthropic, OpenAI, Ollama, Groq, Google)
- Built-in tools (read, write, glob, grep, bash, edit, list)
- MCP client
- Session management (rusqlite)
- Skill system with model configuration

**TUI Stack:**

- ratatui + crossterm with insert_before for scrollback
- Fixed 15-line inline viewport (UI_VIEWPORT_HEIGHT constant)
- Custom Composer (src/tui/composer/) - ropey-backed text buffer
- FilterInput (src/tui/filter_input.rs) - simple single-line for pickers

## Known Issues

| Issue              | Status | Notes                                           |
| ------------------ | ------ | ----------------------------------------------- |
| Cursor off by 2    | Open   | tk-lx9z - multiline input cursor position       |
| Wrapped navigation | Open   | tk-gg9m - up/down should follow visual lines    |
| Newline input      | Closed | Alt+Enter/Ctrl+J fallbacks added (d8c97a7)      |
| Scrollback cut off | Closed | Fixed by removing terminal recreation (cfc3425) |
| Viewport gap       | Closed | Fixed by removing ui_top, render from area.y    |
| Double blank line  | Closed | Fixed by skipping empty TextDelta (2406b91)     |
| Token counting     | Closed | Fixed: context % + cumulative ↑ (9c83a42)       |

## Config System

**Config fields:**

- `provider` - Selected provider ID (openrouter, google, etc.)
- `model` - Model name as the API expects it
- `api_keys` - Optional section for users without env vars

**API key priority:** Config file > Environment variable

## Known Limitations

**Sandbox:**

- `check_sandbox()` is path validation, not true sandboxing.
- True sandboxing is post-MVP.

**Streaming:**

- llm-connector has parse issues with streaming tool calls on some providers.
- OpenRouter and Ollama fall back to non-streaming when tools are present.
