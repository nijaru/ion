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
- Built-in tools (read, write, glob, grep, bash)
- MCP client
- Session management (rusqlite)
- Skill system with model configuration

**Tools Status:**

| Tool  | Status   | Notes                                      |
| ----- | -------- | ------------------------------------------ |
| read  | Done     | Safe, file reading                         |
| write | Done     | Restricted, full file write with diff      |
| glob  | Done     | Safe, pattern matching via ignore crate    |
| grep  | Done     | Safe, content search                       |
| bash  | Done     | Restricted, shell commands                 |
| edit  | **TODO** | String replacement (old_string/new_string) |
| list  | **TODO** | fd-like directory listing                  |

**Providers** (via `llm` crate):

| Provider   | Status | Notes                               |
| ---------- | ------ | ----------------------------------- |
| OpenRouter | Full   | Primary, 200+ models                |
| Anthropic  | Full   | Direct Claude access                |
| OpenAI     | Full   | Has base_url field                  |
| Ollama     | Full   | Auto-discovers at localhost:11434   |
| Groq       | Full   | Fast inference                      |
| Google     | Full   | Falls back to non-streaming w/tools |

## Config System

**Config fields:**

- `provider` - Selected provider ID (openrouter, google, etc.)
- `model` - Model name as the API expects it
- `api_keys` - Optional section for users without env vars

**API key priority:** Config file > Environment variable

**Model ID format:**

- OpenRouter: `anthropic/claude-3-opus` (their API expects this)
- Direct providers: `gemini-3-flash` (native model names)

## Provider System

**Unified `Provider` enum** in `src/provider/api_provider.rs`:

- 6 variants: OpenRouter, Anthropic, OpenAI, Google, Ollama, Groq
- Methods: `id()`, `name()`, `description()`, `env_vars()`, `api_key()`, `is_available()`, `to_llm()`
- `ProviderStatus` for TUI picker with authentication status

## Open Tasks

Run `tk ready` for current task list.

**High Priority:**

1. **Add edit tool** (tk-b4hd) - Critical for efficient editing
2. **Add list tool** (tk-miou) - fd-like, uses ignore crate
3. **Modular streaming interface** (tk-g1fy) - Research needed

**Medium Priority:**

- Model sorting: org → newest → alphabetical (tk-r9c7)
- Diff highlighting for edits (tk-er0v)
- System prompt comparison (tk-gsiw)

**Low Priority / Ideas:**

- OAuth system for providers (tk-t0ea)
- Permission system audit (tk-5h0j)
- OpenRouter provider routing modal (tk-iegz)
- Model display format (tk-x3zf)

## Session Work 2026-01-19

**Completed:**

- Provider persistence: Config now stores `provider` and `model` explicitly
- API key priority: Config > env var (explicit config is more intentional)
- Model ID format: OpenRouter keeps `org/model`, direct providers use native names
- Fixed Google 404 error (was sending `google:model` instead of `model`)
- Streaming+tools fallback for providers that don't support it
- Rate limit (429) retry with exponential backoff
- Tool display: `> **tool_name** (args)` format
- Task completed styling: dim green checkmark
- Removed discover tool (no backend)
- **Unified Provider types** (tk-gpdy): Merged `Backend` and `ApiProvider` into single `Provider` enum

## Design Decisions

**Config over env vars:**

- Explicit user config should take priority
- Env vars are system-wide defaults, not tool-specific intent

**Provider-specific model IDs:**

- OpenRouter: models ARE identified by `org/model`
- Direct providers: use native model names
- Switching providers means re-selecting model (different APIs)

## Known Limitations

**Sandbox:**

- `check_sandbox()` is path validation, not true sandboxing
- Bash commands can access outside CWD
- True sandboxing is post-MVP

**Streaming:**

- llm crate: Some providers don't support streaming+tools
- Falls back to non-streaming (degraded UX but functional)

## Design Documents

- `ai/design/permission-system.md` - CLI flags, modes, sandbox
- `ai/design/config-system.md` - Config hierarchy
- `ai/design/tui.md` - TUI interface spec
- `ai/DECISIONS.md` - Architecture decisions
- `ai/research/edit-tool-patterns-2026.md` - Edit tool research
- `ai/research/rust-llm-crates-2026.md` - LLM crate comparison
