# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-19 |
| Status     | Runnable        | 2026-01-19 |
| Toolchain  | stable          | 2026-01-19 |
| Tests      | 63 passing      | 2026-01-19 |
| Visibility | **PUBLIC**      | 2026-01-19 |

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Unified provider via `llm-connector` crate (OpenRouter, Anthropic, OpenAI, Ollama, Groq, Google)
- Built-in tools (read, write, glob, grep, bash, edit)
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

Run `tk ls` for current task list. **20 open tasks** as of 2026-01-19.

**Blockers:**

| Task    | Issue                                                           |
| ------- | --------------------------------------------------------------- |
| tk-ypde | rat-text added; full migration deferred (current input works)   |
| tk-oohm | Scroll affects text entry instead of chat (needs investigation) |

**UX Polish:**

| Task    | Issue                                         |
| ------- | --------------------------------------------- |
| tk-u2iu | Git diff format: `[main] +28 -11` with colors |
| tk-abvt | Remove dir name from statusline               |
| tk-6u8c | Status line context % and token display       |
| tk-9n78 | Claude Code tool format - bold, spacing       |
| tk-gf23 | Tool output gap between header and result     |
| tk-bboa | Model picker column width/overflow            |
| tk-kzgo | Dim successful tool result lines              |
| tk-0kxp | Tool output symbol mismatch                   |
| tk-hgl2 | Tool output syntax highlighting               |
| tk-nepc | Token counter not updating (non-streaming)    |

**Features (defer until core is solid):**

| Task    | Issue                                      |
| ------- | ------------------------------------------ |
| tk-vzkd | Input history persistence across restarts  |
| tk-kf3r | Interactive prompts - y/n, editor, browser |
| tk-f564 | OAuth support for providers                |
| tk-vsdp | Theme support - customizable colors        |
| tk-iq98 | Syntax highlighting for code blocks        |
| tk-8qwn | Compare system prompts with other agents   |
| tk-hw04 | Remove AI attribution from commit history  |

## Session Work 2026-01-19

**Session 2 - LLM Migration:**

- **llm-connector migration**: Replaced `llm` crate with `llm-connector` for better tool support
- **Non-streaming fallback**: OpenRouter/Ollama now use non-streaming when tools are present (llm-connector has streaming parse issues)
- **Tool output formatting**: Show tail of content (last 5 lines), collapse read/glob/grep, dim overflow indicator
- **Thinking blocks**: Hide content, show `[Reasoning...]` indicator
- **AutoApproveHandler**: Fixed CLI `--yes` flag for restricted tools (bash)
- **rat-text added**: Dependency added for future text input migration

**Session 1 - UX Polish:**

- **CI fixes** (tk-jiu7): Resolved 36 clippy warnings for CI
- **Modal hotkeys** (tk-eesg, tk-a9rd): Ctrl+P/Ctrl+M now work inside pickers
- **Scrolling fix** (tk-z7r2): Rewrote to line-based scrolling with Paragraph.scroll()
- **Final message visibility** (tk-x0z0): Removed dim styling, auto-scroll on completion

**Design notes:**

- Scroll system now tracks lines (not entries) from bottom
- Completion messages now visible green (not dim)
- Auto-scroll to bottom when task finishes so users see result

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
- `ai/DECISIONS.md` - Architecture decisions
- `ai/research/edit-tool-patterns-2026.md` - Edit tool research
