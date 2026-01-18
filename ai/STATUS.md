# ion Status

## Current State

| Metric    | Value           | Updated    |
| --------- | --------------- | ---------- |
| Phase     | 5 - Polish & UX | 2026-01-18 |
| Focus     | Config System   | 2026-01-18 |
| Status    | Runnable        | 2026-01-18 |
| Toolchain | stable          | 2026-01-18 |
| Tests     | 49 passing      | 2026-01-18 |

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

**Memory** (archived to nijaru/ion-archive):

- Will re-implement after TUI agent is fully working

## Session Accomplishments

**Config System Design:**

- Researched CLI agent config best practices (Claude Code, Cursor, aider, goose, OpenCode)
- Designed 3-tier config hierarchy (user → project → local)
- Proposed `~/.agents/` as universal standard for AI agent files
- TOML format, AGENTS.md primary with CLAUDE.md fallback
- Created `ai/design/config-system.md`
- Updated README with new config structure

**Key Decisions:**

- `~/.ion/` for ion config (matches cargo, claude, cursor convention)
- `~/.agents/` for universal shared files (proposed standard)
- First-found wins for instruction files (no concatenation at same level)
- Auto-gitignore `.ion/*.local.toml`

## Open Tasks

**Bugs:**

(none)

**UX:**

- [ ] tk-otmx: Ctrl+G opens input in external editor
- [ ] tk-whde: Git diff stats in status line
- [ ] tk-arh6: Tool execution not visually obvious
- [ ] tk-o4uo: Modal escape handling

**Ideas:**

- [ ] tk-iegz: OpenRouter provider routing modal
- [ ] tk-smqs: Diff highlighting for edits

## Completed

- [x] Config system implementation
- [x] Dependency upgrades (grep, glob, tokenizer)
- [x] Memory removal / stable Rust switch
- [x] Config persistence (model selection saved)
- [x] Progress line with elapsed + token counts
- [x] CLI one-shot mode

## Design Documents

- `ai/design/config-system.md` - Config hierarchy, ~/.agents/ proposal
- `ai/design/tui.md` - TUI interface spec
- `ai/design/diff-highlighting.md` - Diff rendering plan
- `ai/design/interrupt-handling.md` - Ctrl+C behavior spec
- `ai/research/cli-agent-config-best-practices.md` - Config research
- `ai/DECISIONS.md` - All architecture decisions
