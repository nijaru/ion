# ion Status

## Current State

| Metric | Value           | Updated    |
| ------ | --------------- | ---------- |
| Phase  | 5 - Polish & UX | 2026-01-18 |
| Focus  | TUI Agent MVP   | 2026-01-18 |
| Status | Runnable        | 2026-01-18 |
| Tests  | 51 passing      | 2026-01-18 |

### Session Accomplishments

- **Project Renamed**: agnt → ion (crate, repo, all docs, GitHub)
- **ONNX Runtime Fix**: Removed `load-dynamic` to bundle at build time
- **TUI Polish**:
  - Chat: Simple arrows (`< You`, `> ion`)
  - Status line: Claude Code format
  - Git branch detection
  - Loading: "Ionizing..."
  - Help modal: Centered headers, aligned columns
  - Slash commands with aliases
  - Provider/model pickers simplified
  - Fixed all compiler warnings

### Priority: TUI Agent MVP

Memory system deferred until core TUI is stable.

**P0 - Critical Path:**

- [ ] First-time setup flow (no default provider/model)
- [ ] Slash command autocomplete (fuzzy)
- [ ] Growing text entry box

**P1 - Status Line:**

- [ ] Format: `model [provider dim] · % (used/max) · [branch]`
- [ ] Terminal title: `ion <cwd>`
- [ ] Config: `show_model`, `show_provider`, `show_context`, `show_branch`, `show_cwd`
- [ ] Git diff indicators: `+3 -1` (optional)
- [ ] Custom script support (long-term)

**P2 - Context Tracking:**

- [ ] Token count per message (tiktoken-rs)
- [ ] Context max from model metadata
- [ ] Display `26% (52k/200k)` format
- [ ] Update after compaction

**P3 - Session Management:**

- [ ] 30-day retention (configurable)
- [ ] `/resume` command
- [ ] Session browser/picker

**Completed:**

- [x] TUI Modernization: Minimal Claude Code style
- [x] Help modal: One keybinding per line, centered headers
- [x] Hardened Errors: Type-safe error hierarchy
- [x] Context Caching: minijinja render cache

### Architecture Notes

**Text Entry** (`src/tui/mod.rs`):

- Currently fixed `Constraint::Length(3)` (1 line content)
- Need: Dynamic height up to 50% screen

**Session Store** (`src/session/store.rs`):

- SQLite persistence working
- No retention cleanup yet

**Slash Commands** (`src/tui/mod.rs`):

- Current: Exact match on Enter
- Need: Fuzzy autocomplete dropdown, mid-prompt triggers

## Blockers

None.

## Design Decisions

See `ai/DECISIONS.md` for:

- Status Line Architecture
- Slash Command System
- Session Retention Policy
- First-Time Setup Flow
- Memory System Deferral
