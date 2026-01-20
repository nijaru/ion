# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-20 |
| Status     | Runnable        | 2026-01-20 |
| Toolchain  | stable          | 2026-01-20 |
| Tests      | cargo check     | 2026-01-20 |
| Visibility | **PUBLIC**      | 2026-01-20 |

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Unified provider via `llm-connector` crate (OpenRouter, Anthropic, OpenAI, Ollama, Groq, Google)
- Built-in tools (read, write, glob, grep, bash, edit, list)
- MCP client
- Session management (rusqlite)
- Skill system with model configuration

**Tools Status:**

| Tool  | Status     | Notes                                           |
| ----- | ---------- | ----------------------------------------------- |
| read  | Incomplete | Has offset/limit params but unused, needs chunk |
| write | Done       | Restricted, shows diff or line count            |
| glob  | Done       | Safe, pattern matching via ignore crate         |
| grep  | Done       | Safe, content search                            |
| bash  | Done       | Restricted, shell commands                      |
| edit  | Done       | String replacement (old_string/new_string)      |
| list  | Done       | Safe, fd-like directory listing                 |

**Providers** (via `llm-connector` crate):

| Provider   | Status | Notes                                |
| ---------- | ------ | ------------------------------------ |
| Anthropic  | Full   | Direct Claude access                 |
| Google     | Full   | Falls back to non-streaming/tools    |
| Groq       | Full   | Fast inference                       |
| Ollama     | Full   | Non-streaming when tools are present |
| OpenAI     | Full   | Has base_url field                   |
| OpenRouter | Full   | Non-streaming when tools are present |

## Session Work 2026-01-20

**Inline Viewport Migration (complete):**

- Removed alternate screen mode; use `Viewport::Inline` only.
- Bottom-anchored selector shell for provider/model.
- Message formatting: user `>` prefix, no agent header, thinking/system dimmed.
- Queue editing: Up arrow pulls queued messages back into input.
- Fuzzy matching for selector filters and slash command suggestions.
- License updated to PolyForm Shield.

**Research (complete):**

- Codex CLI uses custom `TextArea` and custom `fuzzy_match` (not a crate).
- rat-text provides multi-line `TextArea` with selection, undo/redo, ropey backend.
- tui-input is a small single-line input helper; not sufficient for editor UX.
- fuzzy-matcher is acceptable for list sizes; nucleo is heavier (MPL-2.0).

**Decisions:**

- Inline viewport is the only supported UI mode.
- Status line is minimal (model + context left, `? help` right). No git/cwd.
- Settings UI deferred; config file only for MVP.
- Provider/model selection uses shared bottom selector with tabs.

**Cancellation responsiveness (complete):**

- Cancel now interrupts non-streaming responses and tool execution.
- Second Ctrl+C during a running task exits immediately.

## Config System

**Config fields:**

- `provider` - Selected provider ID (openrouter, google, etc.)
- `model` - Model name as the API expects it
- `api_keys` - Optional section for users without env vars

**API key priority:** Config file > Environment variable

## Known Limitations

**Sandbox:**

- `check_sandbox()` is path validation, not true sandboxing
- Bash commands can access outside CWD
- True sandboxing is post-MVP

**Streaming:**

- llm-connector has parse issues with streaming tool calls on some providers
- OpenRouter and Ollama fall back to non-streaming when tools are present
- Non-streaming works correctly for tool use

## Design Documents

- `ai/design/permission-system.md` - CLI flags, modes, sandbox
- `ai/design/config-system.md` - Config hierarchy
- `ai/design/tui.md` - TUI interface spec
- `ai/design/inline-viewport.md` - Inline viewport layout and migration
- `ai/DECISIONS.md` - Architecture decisions
- `ai/research/edit-tool-patterns-2026.md` - Edit tool research
- `ai/research/input-and-fuzzy-2026.md` - Input + fuzzy matching research
