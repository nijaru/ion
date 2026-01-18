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

### Phase 5: Polish & UX - In Progress

- [x] **TUI Modernization**: Minimal Claude Code style
- [x] **Hardened Errors**: Type-safe error hierarchy
- [x] **Context Caching**: minijinja render cache
- [ ] **Collapsible thinking blocks** (config option)
- [ ] **Settings menu** (`/config`)
- [ ] **First-time setup flow** (no default model)
- [ ] **Recent models** at top of picker
- [ ] **Token/cost display** in status bar

## Blockers

None.
