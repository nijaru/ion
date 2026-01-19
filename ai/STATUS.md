# ion Status

## Current State

| Metric    | Value           | Updated    |
| --------- | --------------- | ---------- |
| Phase     | 5 - Polish & UX | 2026-01-19 |
| Status    | Runnable        | 2026-01-19 |
| Toolchain | stable          | 2026-01-19 |
| Tests     | 64 passing      | 2026-01-19 |

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
| edit  | Done     | String replacement (old_string/new_string) |
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

Run `tk ls` for current task list. **21 open tasks** as of 2026-01-19.

**Critical - Core Bugs:**

| Task    | Issue                                         |
| ------- | --------------------------------------------- |
| tk-z7r2 | Scrolling broken - PageUp/PageDown shows [+N] |
| tk-oohm | Scroll affects text entry instead of chat     |
| tk-eesg | Ctrl+P not working in model modal             |
| tk-a9rd | Ctrl+M not working in provider modal          |
| tk-x0z0 | Missing final agent message after completion  |

**UX Polish:**

| Task    | Issue                                         |
| ------- | --------------------------------------------- |
| tk-u2iu | Git diff format: `[main] +28 -11` with colors |
| tk-abvt | Remove dir name from statusline               |
| tk-6u8c | Status line context % and token display       |
| tk-9n78 | Claude Code tool format - bold, spacing       |
| tk-gf23 | Tool output gap between header and result     |
| tk-bboa | Model picker column width/overflow            |
| tk-sm2h | Color error lines dim red                     |
| tk-kzgo | Dim successful tool result lines              |

**Infrastructure:**

| Task    | Issue                                        |
| ------- | -------------------------------------------- |
| tk-ypde | Add tui-textarea crate for proper text entry |
| tk-jiu7 | Verify CI migrated from Bun to Rust          |
| tk-g1fy | Streaming+tools - llm crate limitation       |
| tk-pj4e | Clean up repo and make public                |

**Features (defer until core is solid):**

| Task    | Issue                                      |
| ------- | ------------------------------------------ |
| tk-vzkd | Input history persistence across restarts  |
| tk-kf3r | Interactive prompts - y/n, editor, browser |
| tk-f564 | OAuth support for providers                |
| tk-vsdp | Theme support - customizable colors        |
| tk-iq98 | Syntax highlighting for code blocks        |
| tk-miou | List tool - fd-like directory listing      |

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
- **Unified Provider types** (tk-gpdy): Merged `Backend` and `ApiProvider` into single `Provider` enum
- **Edit tool** (tk-b4hd): String replacement with old_string/new_string, uniqueness validation, diff output
- **Ollama context length fix** (tk-xe2c): Context is at `{architecture}.context_length`, not `general.context_length`
- **Task tracking audit**: Reviewed session history, found 21 issues to track

**Design notes:**

- Discover tool: Keep as-is, compiler optimizes out dead code
- Memory system: Use hooks (not MCP) for prompt injection and lifecycle events
- List tool (tk-miou): Show ignored files separately, respect gitignore by default
- Statusline: Remove dir name, format git as `[main] +28 -11` with colors
- Tool colors: Mostly white, dim for secondary info, dim red for errors (not alarming)

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
