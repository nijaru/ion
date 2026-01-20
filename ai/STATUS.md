# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-20 |
| Status     | Runnable        | 2026-01-20 |
| Toolchain  | stable          | 2026-01-20 |
| Tests      | 63 passing      | 2026-01-20 |
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

## Open Tasks

Run `tk ls` for current task list. **18 open tasks** as of 2026-01-20.

**Active:**

| Task    | Issue                                        |
| ------- | -------------------------------------------- |
| tk-l9ye | Queue edit behavior: pull all queued messages into input |

**Open:**

| Task    | Issue                                      |
| ------- | ------------------------------------------ |
| tk-w6id | Fuzzy search for @ and slash commands      |
| tk-l9ye | Queue edit behavior (pull all queued)      |
| tk-p23w | Dependency audit after inline refactor     |
| tk-4cbl | Fuzzy search in selector shell             |
| tk-wsia | Onboarding selector routing                |
| tk-xhkj | Provider/model selector command routing    |
| tk-dd4f | Message formatting update                  |
| tk-d2hx | Selector shell UI                          |
| tk-g82p | Inline refactor: remove alternate screen   |
| tk-pcnt | Research: codex/pi/opencode/CC tools       |
| tk-imza | ast-grep integration                       |
| tk-su1n | Large file handling - chunked reads        |
| tk-1rfr | Web fetch tool                             |
| tk-1y3g | Web search tool                            |
| tk-g063 | @ file inclusion syntax                    |
| tk-hwn1 | BUG: Scroll bounds - past top of chat      |
| tk-hw04 | Remove AI attribution from commit history  |

## Session Work 2026-01-20

**Session - Planning & Design Consolidation:**

- **Docs alignment**: Updated `ai/DESIGN.md` and `ai/design/tui.md` to inline viewport direction
- **Inline UI design**: Consolidated selector shell, onboarding, message formatting in `ai/design/inline-viewport.md`
- **Input/fuzzy research**: Chose rat-text and fuzzy-matcher; added research notes
- **Status line**: Removed git info code paths from TUI
- **Tasks**: Added atomic tasks for selector shell, onboarding, queue edits, fuzzy matching

**Design notes:**

- Inline viewport is primary; alternate screen removal planned
- Status line is minimal: model + context left, `? help` right
- User messages use `>` prefix; system notices dim + bracketed

**Session - Inline Refactor (in progress):**

- Dropped alternate screen setup and switched TUI to inline viewport in `src/main.rs`

**Session - Selector Shell:**

- Replaced provider/model modals with a bottom-anchored selector shell

**Session - Message Formatting:**

- Switching chat rendering to user `>` prefixes, agent no-header, dimmed thinking/system notices

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
