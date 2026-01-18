# ion Status

## Current State

| Metric    | Value             | Updated    |
| --------- | ----------------- | ---------- |
| Phase     | 5 - Polish & UX   | 2026-01-18 |
| Focus     | Permission System | 2026-01-18 |
| Status    | Runnable          | 2026-01-18 |
| Toolchain | stable            | 2026-01-18 |
| Tests     | 54 passing        | 2026-01-18 |

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Built-in providers (OpenRouter, Anthropic, Ollama)
- Built-in tools (read, write, edit, bash, glob, grep)
- MCP client
- Session management (rusqlite)

**Config** (implemented):

- `~/.ion/` for ion-specific config
- `~/.agents/` proposed universal standard for AI agent files
- AGENTS.md + CLAUDE.md instruction file support
- 3-tier layered loading (user → project → local)

**Permissions** (designed, not yet implemented):

- Read/Write/AGI modes
- CWD sandbox by default
- Per-command bash approval

**Memory** (archived to nijaru/ion-archive):

- Will re-implement after TUI agent is fully working

## Session Accomplishments

**Config System Implementation:**

- Replaced `directories` crate with `dirs`
- Changed config path from `~/.config/ion/` to `~/.ion/`
- Added 3-tier layered config loading
- Added AGENTS.md + CLAUDE.md instruction loading
- Added migration from old config location

**Permission System Design:**

- Designed Read/Write/AGI mode system
- CLI flags: `-r`, `-w`, `-y`, `--no-sandbox`, `--agi`
- CWD sandbox by default (84% prompt reduction like Claude Code)
- Per-command bash approval storage
- Created `ai/design/permission-system.md`

**UX Fixes:**

- Ctrl+C immediately cancels running task (no double-tap)
- Modal escape always works (re-opens if setup needed)
- Progress line shows tool name when executing

## Open Tasks

**Security:**

- [ ] tk-a8vn: Permission system implementation (design complete)

**UX:**

- [ ] tk-otmx: Ctrl+G opens input in external editor
- [ ] tk-whde: Git diff stats in status line

**Ideas:**

- [ ] tk-iegz: OpenRouter provider routing modal
- [ ] tk-smqs: Diff highlighting for edits

## Completed This Session

- [x] Config system implementation (tk-e96r)
- [x] Ctrl+C interrupt fix (tk-3jba)
- [x] Modal escape handling (tk-o4uo)
- [x] Tool execution visibility (tk-arh6)
- [x] Permission system design

## Design Documents

- `ai/design/permission-system.md` - CLI flags, modes, sandbox, approval
- `ai/design/config-system.md` - Config hierarchy, ~/.agents/ proposal
- `ai/design/tui.md` - TUI interface spec
- `ai/design/diff-highlighting.md` - Diff rendering plan
- `ai/design/interrupt-handling.md` - Ctrl+C behavior spec
- `ai/research/cli-agent-config-best-practices.md` - Config research
- `ai/DECISIONS.md` - All architecture decisions
