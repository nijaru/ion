# ion Status

## Current State

| Metric | Value           | Updated    |
| ------ | --------------- | ---------- |
| Phase  | 5 - Polish & UX | 2026-01-18 |
| Focus  | TUI Agent MVP   | 2026-01-18 |
| Status | Runnable        | 2026-01-18 |
| Tests  | 51 passing      | 2026-01-18 |

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Built-in providers (OpenRouter, Anthropic, Ollama)
- Built-in tools (read, write, edit, bash, glob, grep)
- MCP client
- Session management
- **Claude Code-compatible hook system**

**Memory Plugin** (ion-memory, separate):

- OmenDB integration
- Loaded via hook system
- Can be skipped for minimal agent

## Session Accomplishments

- First-time setup flow (blocks until provider/model selected)
- Shift+Tab for mode toggle (safer than Tab)
- Help modal: "Ctrl" instead of "^"
- Ctrl+T thinking toggle (off/low/med/high, displays [level] in input box)
- Growing text entry box (3-10 lines)
- Status line with context % display

## Priority: TUI Agent MVP

**P0 - Critical Path:** ✓ COMPLETE

- [x] First-time setup flow (no default provider/model)
- [x] Shift+Tab for mode toggle (safer)
- [x] Help modal: "Ctrl" instead of "^"
- [x] Ctrl+T thinking toggle (off/low/med/high)
- [x] Growing text entry box
- [x] Status line with context %

**P1 - Polish:**

- [x] Status line: `model · %% · [branch] · cwd`
- [x] Thinking display: `[low]` / `[med]` / `[high]` right side of input
- [ ] Terminal title: `ion <cwd>`

**P2 - Features:**

- [ ] Slash command autocomplete (fuzzy)
- [x] Context tracking (tokens used/max) - shows % in status line
- [ ] Session retention (30 days)

**P3 - Plugin System:**

- [ ] Hook event enum
- [ ] Hook runner (subprocess, JSON stdin/stdout)
- [ ] Plugin discovery (.ion/plugins/, ~/.config/ion/plugins/)
- [ ] MCP server loading from plugins

**P4 - Memory Plugin:**

- [ ] Port OmenDB memory hooks
- [ ] Or use existing TypeScript plugin via Bun

## Completed

- [x] TUI Modernization: Minimal Claude Code style
- [x] Help modal: One keybinding per line, centered headers
- [x] Plugin architecture design (`ai/design/plugin-architecture.md`)
- [x] Hardened Errors: Type-safe error hierarchy
- [x] Context Caching: minijinja render cache

## Design Documents

- `ai/design/plugin-architecture.md` - Hook system, plugin format
- `ai/DECISIONS.md` - All architecture decisions

## Blockers

None.
