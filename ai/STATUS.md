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

**Sprint 1 Complete** - Inline Viewport Stabilization done

Verified this session:

1. **No alternate-screen behavior** - Inline viewport preserves native scrollback
2. **Viewport spacing** - Chat in scrollback, UI at bottom, no gaps
3. **Message margins** - 1-column left margin on chat messages
4. **Render helpers** - draw() split into layout_areas, render_progress, etc.
5. **Chat renderer module** - ChatRenderer::build_lines in separate module
6. **History navigation** - Up/Down work correctly at line boundaries

Next: Sprint 2 (Run State UX & Error Handling) or investigate tk-4r7r (scrollback bug)

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Unified provider via `llm-connector` crate (OpenRouter, Anthropic, OpenAI, Ollama, Groq, Google)
- Built-in tools (read, write, glob, grep, bash, edit, list)
- MCP client
- Session management (rusqlite)
- Skill system with model configuration

**TUI Stack:**

- ratatui + crossterm (kept for insert_before scrollback)
- Custom Composer (src/tui/composer/) - ropey-backed text buffer
- FilterInput (src/tui/filter_input.rs) - simple single-line for pickers
- Full-height inline viewport with UI at bottom

## Known Issues

| Issue              | Status | Notes                                |
| ------------------ | ------ | ------------------------------------ |
| Scrollback cut off | Open   | Related to viewport issues (tk-4r7r) |
| Shift+Enter issues | Closed | Part of keybindings (tk-etpd)        |

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
