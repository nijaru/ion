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

Completed this session:

1. **Sprint 1 verification** - All acceptance criteria met
2. **Scrollback fix** - Removed terminal recreation on resize (cfc3425)
3. **Unicode width fix** - count_wrapped_lines uses display width

Next: Sprint 2 (Run State UX & Error Handling)

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

| Issue              | Status | Notes                                           |
| ------------------ | ------ | ----------------------------------------------- |
| Scrollback cut off | Closed | Fixed by removing terminal recreation (cfc3425) |
| Shift+Enter issues | Closed | Part of keybindings (tk-etpd)                   |

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
