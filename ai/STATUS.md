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

**Sprint 0 Complete** - TUI Architecture overhaul done

Completed this session:

1. **Custom Composer** - Ported from ion-copy (ropey + unicode-segmentation)
2. **Platform keybindings** - macOS Option, Win/Linux Ctrl for word ops
3. **Dynamic input height** - Grows to term_height - 6
4. **Full-height viewport** - Never recreate except on terminal resize
5. **Removed rat-text** - Fully replaced with custom Composer + FilterInput

Next: Sprint 1 (Inline Viewport Stabilization) or verify scrollback (tk-4r7r)

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
