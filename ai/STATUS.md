# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-23 |
| Status     | Runnable        | 2026-01-23 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | cargo check     | 2026-01-23 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Work

**Viewport fixes in progress** - Several rounds of debugging inline viewport behavior.

Session accomplishments:

- **Gap fix** - UI now renders from top of viewport, not bottom (removed ui_top calculation)
- **Ctrl+G crash fix** - Editor errors handled gracefully, won't exit app
- **Viewport 15 lines** - Increased from 10 to allow more input growth
- **MIN_RESERVED 3** - Reduced from 6, allows ~10 content lines in input
- **Codex research** - Analyzed their viewport patterns for reference

Remaining issues (new tasks created):

- tk-lx9z: Cursor position off by 2 chars in multiline
- tk-gg9m: Up/down should navigate wrapped visual lines
- tk-ucw5: Token counter should reset per turn

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
| Token counter      | Open   | tk-ucw5 - resets cumulative, should be per-turn |
| Scrollback cut off | Closed | Fixed by removing terminal recreation (cfc3425) |
| Viewport gap       | Closed | Fixed by removing ui_top, render from area.y    |

## Codex CLI Patterns (Reference)

From `/Users/nick/github/openai/codex`:

- **No insert_before** - Uses flex layout for everything in one viewport
- **desired_height(width)** - Each widget calculates wrapped height
- **Wrapped line cache** - Caches wrapped lines per width using textwrap
- **preferred_col** - Maintains horizontal position when moving up/down
- **Grapheme width** - Uses unicode_width for cursor column calculation

Key files:

- `codex-rs/tui/src/bottom_pane/textarea.rs` - Input with wrapping
- `codex-rs/tui/src/render/renderable.rs` - Layout composition

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
