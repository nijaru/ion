# ion Status

## Current State

| Metric | Value           | Updated    |
| ------ | --------------- | ---------- |
| Phase  | 5 - Polish & UX | 2026-01-18 |
| Focus  | TUI UX Polish   | 2026-01-18 |
| Status | Runnable        | 2026-01-18 |
| Tests  | 51 passing      | 2026-01-18 |

### Accomplishments This Session

- **Project Renamed**: agnt → ion (crate, repo, all docs, GitHub)
- **ONNX Runtime Fix**: Removed `load-dynamic` from ort to bundle at build time
- **TUI Polish**:
  - Chat: Simple arrows (`< You`, `> ion`) instead of Nerd Font icons
  - Status line: Claude Code format (`model · [branch] · cwd` left, `? help` right)
  - Git branch detection via `.git/HEAD`
  - Loading indicator: "Ionizing..."
  - Help modal: Fixed size (50x18), centered, keybindings + commands
  - `?` triggers help when input empty
  - Slash commands: `/model`, `/provider`, `/quit`, `/exit`, `/q`, `/?`
  - Provider picker: Simplified (name + auth hint only), Ctrl+W word delete
  - Model picker: Fixed filter (was blocking all models)
  - 1-space left padding throughout
  - Fixed all compiler warnings

### Phase 5: Polish & UX - In Progress

**Completed:**

- [x] TUI Modernization: Minimal Claude Code style
- [x] Hardened Errors: Type-safe error hierarchy
- [x] Context Caching: minijinja render cache

**Status Line:**

- [ ] Format: `model [provider dim] · % (used/max) · [branch] · cwd`
- [ ] Configurable settings (show/hide: model, provider, context%, raw tokens, branch, cwd)
- [ ] Set terminal title to `ion <cwd>` (remove cwd from status line?)
- [ ] Long-term: custom script hook (like Claude Code)

**Context Tracking:**

- [ ] Track token count per message (tiktoken-rs)
- [ ] Get context max from model metadata
- [ ] Display `26% (52k/200k)` format
- [ ] Update after compaction

**Text Entry:**

- [ ] Growing text box (currently fixed 3 lines)
- [ ] Grow up to ~50% of screen height (like Claude Code)
- [ ] Handle cursor position in multiline

**Help Modal:**

- [ ] One keybinding per line (not multiple on same line)

**First-Time UX:**

- [ ] No default provider - require selection
- [ ] No default model - require selection
- [ ] Settings menu (`/config` or `/settings`)

**Session Management:**

- [ ] Conversation retention policy (30 days default?)
- [ ] Research: continue vs resume usage patterns
- [ ] Session browser/picker

**Other:**

- [ ] Collapsible thinking blocks (config option)
- [ ] Recent models at top of picker
- [ ] Cost tracking

## Design Decisions Needed

1. **CWD in status line**: Worth having? Or just terminal title?
2. **Status line settings**: Live TUI menu vs config file only?
3. **Retention period**: 30 days? Configurable?

## Current Architecture

**Session Store** (`src/session/store.rs`):

- SQLite-based persistence
- Incremental message saves
- Session listing with first user message preview
- No retention cleanup yet

**Text Entry** (`src/tui/mod.rs`):

- Fixed `Constraint::Length(3)` (3 lines total, 1 content)
- Shift+Enter for newlines supported
- No dynamic height

## Blockers

None.
