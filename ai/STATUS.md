# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-19 |
| Status     | Runnable        | 2026-01-19 |
| Toolchain  | stable          | 2026-01-19 |
| Tests      | 64 passing      | 2026-01-19 |
| Visibility | **PUBLIC**      | 2026-01-19 |

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Unified provider via `llm` crate (OpenRouter, Anthropic, OpenAI, Ollama, Groq, Google)
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

**Providers** (via `llm` crate):

| Provider   | Status | Notes                             |
| ---------- | ------ | --------------------------------- |
| Anthropic  | Full   | Direct Claude access              |
| Google     | Full   | Falls back to non-streaming/tools |
| Groq       | Full   | Fast inference                    |
| Ollama     | Full   | Auto-discovers at localhost:11434 |
| OpenAI     | Full   | Has base_url field                |
| OpenRouter | Full   | 200+ models aggregator            |

## Open Tasks

Run `tk ls` for current task list. **20 open tasks** as of 2026-01-19.

**Blockers:**

| Task    | Issue                                                           |
| ------- | --------------------------------------------------------------- |
| tk-ypde | tui-textarea blocked on ratatui 0.30 compatibility (uses 0.29)  |
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
| tk-sm2h | Color error lines dim red                     |
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

**Completed this session:**

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

- llm crate: Some providers don't support streaming+tools
- Falls back to non-streaming (degraded UX but functional)

## Design Documents

- `ai/design/permission-system.md` - CLI flags, modes, sandbox
- `ai/design/config-system.md` - Config hierarchy
- `ai/design/tui.md` - TUI interface spec
- `ai/DECISIONS.md` - Architecture decisions
- `ai/research/edit-tool-patterns-2026.md` - Edit tool research
