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

## Open Tasks

Run `tk ready` for current task list. Key items:

| ID      | Category | Description                       |
| ------- | -------- | --------------------------------- |
| tk-kj66 | UX       | Shift+Enter multiline input       |
| tk-av8a | UX       | Status line token/context display |
| tk-j7io | UX       | Progress bar completion behavior  |
| tk-otmx | UX       | Ctrl+G external editor            |
| tk-whde | UX       | Git diff stats in status line     |
| tk-6zlg | Config   | Thinking budget levels            |
| tk-usd5 | Infra    | CI migration (Bun → Rust)         |
| tk-smqs | Idea     | Diff highlighting                 |
| tk-iegz | Idea     | OpenRouter routing modal          |

**Untracked ideas:**

- Interactive shell support (ai/ideas/interactive-shell.md)

## Known Limitations

**Sandbox:**

- `check_sandbox()` is path validation, not true sandboxing
- Bash commands can still access outside CWD (run with `current_dir` set but can `cd` or use absolute paths)
- True sandboxing (containers/chroot) is post-MVP

## Design Documents

- `ai/design/permission-system.md` - CLI flags, modes, sandbox, approval
- `ai/design/config-system.md` - Config hierarchy, ~/.agents/ proposal
- `ai/design/tui.md` - TUI interface spec
- `ai/design/diff-highlighting.md` - Diff rendering plan
- `ai/design/interrupt-handling.md` - Ctrl+C behavior spec
- `ai/research/cli-agent-config-best-practices.md` - Config research
- `ai/DECISIONS.md` - All architecture decisions
