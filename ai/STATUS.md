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

| Tool     | Status   | Notes                                      |
| -------- | -------- | ------------------------------------------ |
| read     | Done     | Safe, file reading                         |
| write    | Done     | Restricted, full file write with diff      |
| glob     | Done     | Safe, pattern matching via ignore crate    |
| grep     | Done     | Safe, content search                       |
| bash     | Done     | Restricted, shell commands                 |
| edit     | **TODO** | String replacement (old_string/new_string) |
| list     | **TODO** | fd-like directory listing                  |
| discover | Remove   | Placeholder with no backend                |

**Providers** (via `llm` crate):

| Provider   | Status  | Notes                             |
| ---------- | ------- | --------------------------------- |
| OpenRouter | Full    | Primary, 200+ models              |
| Anthropic  | Full    | Direct Claude access              |
| OpenAI     | Full    | Has base_url field                |
| Ollama     | Full    | Auto-discovers at localhost:11434 |
| Groq       | Full    | Fast inference                    |
| Google     | Partial | **BUG: API key not persisted**    |

## Active Bugs

**Provider persistence (tk-rrue):**

- Config only stores `openrouter_api_key` and `anthropic_api_key`
- No Google/Groq/etc API key storage
- On restart, provider derived from stored keys, not from model string
- Model `google:gemini-3-flash-preview` loses provider on restart
- **Fix**: Parse provider from model string, or add all API keys to config

**Streaming with tools (FIXED this session):**

- Google provider errors with "streaming with tools not supported"
- Added fallback: catch error, retry with non-streaming
- Works for any provider that lacks streaming+tools support

## Open Tasks

Run `tk ready` for current task list.

**High Priority:**

1. **Provider persistence bug** (tk-rrue) - Blocks Google/Groq usage
2. **Streaming+tools support** (tk-e1ji) - llm crate doesn't support for Google, need custom or better crate
3. **Add edit tool** (tk-b4hd) - Critical for efficient editing
4. **Add list tool** (tk-miou) - fd-like, uses ignore crate

**Medium Priority:**

5. Model sorting: org → newest → alphabetical (tk-r9c7)
6. Diff highlighting for edits (tk-er0v)
7. System prompt comparison (tk-gsiw)

**Low Priority / Ideas:**

- OAuth system for providers (tk-t0ea)
- Permission system audit (tk-5h0j)
- OpenRouter provider routing modal (tk-iegz)
- Model display format - just model name vs provider:model (tk-x3zf)

## Session Work 2026-01-19

**Completed:**

- Rate limit (429) retry with exponential backoff (2s, 4s, 8s)
- Ollama context lengths fetched from `/api/show` endpoint
- Streaming+tools fallback for providers that don't support it
- Tool display: `> **tool_name** (args)` + `└ result` format
- Task completed styling: dim green `✓` instead of yellow `!`
- Bold tool names, `>` prefix, proper spacing
- Removed discover tool from registration (no backend)

**Identified Issues:**

- Provider resets to OpenRouter on restart
- Config missing API keys for Google, Groq, etc.
- No edit tool (only write which rewrites entire file)
- No list/find tool (glob is pattern-only)
- llm crate quirks: streaming+tools, system messages as user

**Research Completed:**

- `ai/research/edit-tool-patterns-2026.md` - All agents use string replacement
- `ai/research/rust-file-finder-crates.md` - Use ignore crate (fd's backend)
- `ai/research/rust-llm-crates-2026.md` - llm crate vs alternatives

## Design Decisions Pending

**llm crate vs custom (PRIORITY):**

- Tools are ALWAYS present for coding agent - streaming+tools is primary use case
- llm crate: Google doesn't support streaming+tools, falls back to non-streaming (degraded UX)
- Non-streaming = no incremental text output, user sees nothing until complete
- Options:
  1. Find crate that supports streaming+tools for all providers
  2. Build custom (500-1000 LOC/provider, full control)
  3. Contribute fix to llm crate
- This is a **blocker** for good Google/other provider support

**Permission model:**

- Current: 3 modes (Read/Write/AGI) with approval handler
- Pi-Mono uses YOLO (no approvals) - "security theater"
- Keep current model, audit for issues

**Tool display colors:**

- While running: dim text
- Completed: normal white
- Success: dim green checkmark
- Error: red

## Known Limitations

**Sandbox:**

- `check_sandbox()` is path validation, not true sandboxing
- Bash commands can still access outside CWD
- True sandboxing (containers/chroot) is post-MVP

**Providers:**

- Uses `llm` crate for unified backend
- Some providers need streaming fallback when tools present
- vLLM/mlx-lm need config file for custom endpoints
- Vertex AI not yet supported (Google AI Studio works)

## Design Documents

- `ai/design/permission-system.md` - CLI flags, modes, sandbox
- `ai/design/config-system.md` - Config hierarchy
- `ai/design/tui.md` - TUI interface spec
- `ai/design/plugin-architecture.md` - Plugin/MCP design
- `ai/design/model-listing-refactor.md` - Model listing cleanup
- `ai/DECISIONS.md` - All architecture decisions
