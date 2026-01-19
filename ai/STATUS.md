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
- Unified provider via `llm` crate (OpenRouter, Anthropic, OpenAI, Ollama, Groq, Google)
- Built-in tools (read, write, edit, bash, glob, grep)
- MCP client
- Session management (rusqlite)
- Skill system with model configuration

**Providers** (via `llm` crate):

| Provider   | Status | Notes                             |
| ---------- | ------ | --------------------------------- |
| OpenRouter | Full   | Primary, 200+ models              |
| Anthropic  | Full   | Direct Claude access              |
| OpenAI     | Full   | Has base_url field                |
| Ollama     | Full   | Auto-discovers at localhost:11434 |
| Groq       | Full   | Fast inference                    |
| Google     | Full   | Gemini via AI Studio              |
| vLLM       | None   | Config: OpenAI-compatible         |
| mlx-lm     | None   | Config: OpenAI-compatible         |

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

**Priority (Refactor):**

1. Model listing refactor - move to registry (see ai/design/model-listing-refactor.md)
2. OpenAI-compatible endpoint config (vLLM, mlx-lm)

**Lower Priority (Polish):**

- Diff highlighting for edits

**Ideas:**

- Interactive shell support (ai/ideas/interactive-shell.md)

## Recent Session Work

- Fixed OpenRouter feature not enabled in llm crate
- Implemented model listing for Ollama and OpenRouter
- Improved tool call display in chat history
- Added provider name to model picker title
- Code review: fixed UTF-8 panic, CLI permission consistency
- Designed model listing refactor (ai/design/model-listing-refactor.md)

## Known Limitations

**Sandbox:**

- `check_sandbox()` is path validation, not true sandboxing
- Bash commands can still access outside CWD
- True sandboxing (containers/chroot) is post-MVP

**Providers:**

- Uses `llm` crate for unified backend (streaming, tool calling)
- vLLM/mlx-lm need config file for custom OpenAI-compatible endpoints
- Vertex AI not yet supported (Google AI Studio works)

## Design Documents

- `ai/design/permission-system.md` - CLI flags, modes, sandbox
- `ai/design/config-system.md` - Config hierarchy
- `ai/design/tui.md` - TUI interface spec
- `ai/design/plugin-architecture.md` - Plugin/MCP design
- `ai/design/model-listing-refactor.md` - Model listing cleanup
- `ai/DECISIONS.md` - All architecture decisions
