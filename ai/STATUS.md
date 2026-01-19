# ion Status

## Current State

| Metric    | Value           | Updated    |
| --------- | --------------- | ---------- |
| Phase     | 5 - Polish & UX | 2026-01-19 |
| Status    | Runnable        | 2026-01-19 |
| Toolchain | stable          | 2026-01-19 |
| Tests     | 57 passing      | 2026-01-19 |

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Built-in providers (OpenRouter, Anthropic, OpenAI)
- Built-in tools (read, write, edit, bash, glob, grep)
- MCP client
- Session management (rusqlite)
- Skill system with model configuration

**Providers** (status):

| Provider   | Status | Notes                |
| ---------- | ------ | -------------------- |
| OpenRouter | Full   | Primary, 200+ models |
| Anthropic  | Full   | Direct Claude access |
| OpenAI     | Full   | Has base_url field   |
| Ollama     | Stub   | Needs implementation |
| vLLM       | None   | OpenAI-compatible    |
| mlx-lm     | None   | OpenAI-compatible    |

**Config** (implemented):

- `~/.ion/` for ion-specific config
- AGENTS.md + CLAUDE.md instruction file support
- 3-tier layered loading (user -> project -> local)

**Permissions** (implemented):

- Read/Write/AGI modes via CLI flags (-r, -w, --agi)
- CWD sandbox by default (--no-sandbox to disable)
- Per-command bash approval storage

## Open Tasks

Run `tk ready` for current task list.

**Priority:**

- OpenAI-compatible provider (Ollama, vLLM, mlx-lm support)
- Diff highlighting for edits

**Ideas:**

- Interactive shell support (ai/ideas/interactive-shell.md)

## Known Limitations

**Sandbox:**

- `check_sandbox()` is path validation, not true sandboxing
- Bash commands can still access outside CWD
- True sandboxing (containers/chroot) is post-MVP

**Providers:**

- Ollama defined but not implemented (falls back to OpenRouter)
- No custom endpoint support yet

## Design Documents

- `ai/design/permission-system.md` - CLI flags, modes, sandbox
- `ai/design/config-system.md` - Config hierarchy
- `ai/design/tui.md` - TUI interface spec
- `ai/design/plugin-architecture.md` - Plugin/MCP design
- `ai/DECISIONS.md` - All architecture decisions
