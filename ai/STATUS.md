# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-22 |
| Status     | Runnable        | 2026-01-22 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | cargo check     | 2026-01-22 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Unified provider via `llm-connector` crate (OpenRouter, Anthropic, OpenAI, Ollama, Groq, Google)
- Built-in tools (read, write, glob, grep, bash, edit, list)
- MCP client
- Session management (rusqlite)
- Skill system with model configuration

**Tools Status:**

| Tool  | Status | Notes                                           |
| ----- | ------ | ----------------------------------------------- |
| read  | Incomp | Has offset/limit params but unused, needs chunk |
| write | Done   | Restricted, shows diff or line count            |
| glob  | Done   | Upgraded to globset via ignore crate            |
| grep  | Done   | Upgraded to ignore crate                        |
| bash  | Done   | Restricted, shell commands                      |
| edit  | Done   | String replacement (old_string/new_string)      |
| list  | Done   | Safe, fd-like directory listing                 |

## Session Work 2026-01-22

**Sprint Planning & Consolidation:**

- Merged new project specifications and design docs into a unified `ai/SPRINTS.md`.
- Sprints reorganized into: Stabilization, Persistence, and Advanced Polish.
- Added dependency upgrade tasks (ignore, globset, bpe-openai) to the roadmap.
- Validated task status against `.tasks/` store.

**Recent fixes (complete):**

- Chat output is append-only via `Terminal::insert_before`; viewport redraws progress/input/status only.
- User message prefix shown only on the first line; user text dimmed cyan.
- History navigation now skips phantom blank entries; up/down recall works on first press.
- Input box uses rounded borders (Block + `BorderType::Rounded`).
- Write/edit allowed in write mode; restricted tools require approval unless whitelisted.
- `chat_lines` order fixed in draw; cargo check passes.
- UTF-8 safe truncation in CLI/TUI display paths.
- NaN-safe pricing sort in model registry.

**Decisions:**

- Use `ai/SPRINTS.md` as the single source of truth for task tracking alongside `tk`.
- Prioritize inline viewport stabilization before adding session resumption UI.

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

**UX:**

- Viewport height calculations need tightening to avoid blank gaps.
- Input editor phantom line during multi-line typing.