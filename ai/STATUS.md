# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-23 |
| Status     | Runnable        | 2026-01-23 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | cargo check     | 2026-01-22 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Active Work

**TUI Overhaul** - Custom text entry + viewport fix

Plan: `/Users/nick/.claude/plans/merry-knitting-crab.md`

Key decisions made this session:

1. **Keep ratatui** - `insert_before()` for native scrollback is critical
2. **Custom Composer** - Port from ion-copy (ropey + unicode-segmentation), not reedline
3. **Full-height viewport** - Never recreate except on terminal resize
4. **Dynamic input height** - Grows to terminal height minus reserved (6 lines)

Source code to port: `../ion-copy/src/tui/widgets/composer/`

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Unified provider via `llm-connector` crate (OpenRouter, Anthropic, OpenAI, Ollama, Groq, Google)
- Built-in tools (read, write, glob, grep, bash, edit, list)
- MCP client
- Session management (rusqlite)
- Skill system with model configuration

## Known Issues

| Issue                    | Status  | Notes                                  |
| ------------------------ | ------- | -------------------------------------- |
| Viewport content leaking | Planned | Full-height viewport fix (tk-qo7b)     |
| rat-text incompatible    | Planned | Replace with custom Composer (tk-l6yf) |
| Shift+Enter not working  | Open    | Check Kitty protocol (tk-etpd)         |
| Scrollback cut off       | Open    | Related to viewport issues (tk-4r7r)   |

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
