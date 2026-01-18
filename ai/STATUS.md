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

- Provider picker: compact modal, column layout, Ctrl+C quits
- Model picker: flat list of all models, sorted by provider/name
- Model picker: column layout (name, provider dim, context, price)
- First-time setup: Esc goes back to provider picker
- Error display in status line
- Debug logging with ION_LOG=1 env var

## Priority: TUI Agent MVP

**P0 - Critical Path:** COMPLETE

**P1 - Polish:**

- [x] Status line: `model · %% · [branch] · cwd`
- [x] Thinking display: `[low]` / `[med]` / `[high]` right side of input
- [ ] Terminal title: `ion <cwd>`
- [ ] Model picker: fix pricing display (all show $0.00)

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
- [x] First-time setup flow
- [x] Provider/Model picker UX overhaul

## Design Documents

- `ai/design/plugin-architecture.md` - Hook system, plugin format
- `ai/DECISIONS.md` - All architecture decisions

## Blockers

- Model picker pricing: OpenRouter API returns prices but parsing shows $0.00
  - Need to investigate API response format (string vs number)
  - Special models (auto, bodybuilder) filtered but may be others
