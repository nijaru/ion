# ion Status

## Current State

| Metric    | Value           | Updated    |
| --------- | --------------- | ---------- |
| Phase     | 5 - Polish & UX | 2026-01-19 |
| Focus     | UX Polish       | 2026-01-19 |
| Status    | Runnable        | 2026-01-19 |
| Toolchain | stable          | 2026-01-19 |
| Tests     | 54 passing      | 2026-01-19 |

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

**Permissions** (implemented):

- Read/Write/AGI modes via CLI flags (-r, -w, --agi)
- CWD sandbox by default (--no-sandbox to disable)
- Auto-approve via -y/--yes
- Per-command bash approval storage
- Config file support for defaults

**Memory** (archived to nijaru/ion-archive):

- Will re-implement after TUI agent is fully working

## Session Accomplishments

**Permission System Implementation (tk-a8vn):**

- CLI flags: `-r`/`--read`, `-w`/`--write`, `-y`/`--yes`, `--no-sandbox`, `--agi`
- CWD boundary checking via `ToolContext.check_sandbox()`
- Per-command bash approval (not just per-tool)
- TUI mode cycling respects --agi flag (Read ↔ Write unless --agi)
- Config file support: `[permissions]` section
- Flag warnings for invalid combinations (-r -y)

## Open Tasks

**UX:**

- [ ] tk-otmx: Ctrl+G opens input in external editor
- [ ] tk-whde: Git diff stats in status line

**Ideas:**

- [ ] tk-iegz: OpenRouter provider routing modal
- [ ] tk-smqs: Diff highlighting for edits
- [ ] Interactive shell support (ai/ideas/interactive-shell.md)

## Completed This Session

- [x] Permission system implementation (tk-a8vn)

## Design Documents

- `ai/design/permission-system.md` - CLI flags, modes, sandbox, approval
- `ai/design/config-system.md` - Config hierarchy, ~/.agents/ proposal
- `ai/design/tui.md` - TUI interface spec
- `ai/design/diff-highlighting.md` - Diff rendering plan
- `ai/design/interrupt-handling.md` - Ctrl+C behavior spec
- `ai/research/cli-agent-config-best-practices.md` - Config research
- `ai/DECISIONS.md` - All architecture decisions
