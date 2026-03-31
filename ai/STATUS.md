# Status: ion (Go)

Fast, lightweight terminal coding agent.

## Current Focus
`tk-canto-refactor` (Canto/Ion Architectural Alignment) — Migrating core safety and tool primitives to the framework, modularizing the TUI.

## Next Steps
1. **`tk-canto-refactor`** — Move `PolicyEngine` to `canto/safety`, migrate standard tools (`bash`, `file`, `grep`, `glob`) to `canto/x/tools`.
2. **`tk-ion-tui-modular`** — Componentize `internal/app/model.go` into `Viewport`, `Input`, and `Broker` sub-models.
3. **`tk-canto-governor`** — Implement background "Context Governor" in `canto` for automated soft compaction.
4. **`tk-k4hv`** — Complete 3-mode system (READ/EDIT/YOLO) integration with the new `canto/safety` engine.

## Completed (Recent)
- [x] **Architecture Review** — SOTA alignment with Claude Code and Pi-mono. Defined Layer 3 (Canto) and Layer 4 (Ion) boundaries.
- [x] **Approval UX Spec (`tk-k4hv`)** — 3-mode design (READ/EDIT/YOLO) documented.
- [x] **Agent Compaction Tool (`tk-pw3s`)** — `compact` tool implemented in Ion.
- [x] **Canto Migration Complete** — Pinned to `github.com/nijaru/canto`.

## Active Tasks
- `tk-2b79` [p2 active] Modularize Ion TUI (Viewport, Input, Broker refactored)
- `tk-k4hv` [p2 active] Approval UX overhaul + `/yolo`
- `tk-8s0h` [p2 open] Sandbox support
- `tk-k8g4` [p3 open] Model selector: add hotkey and UI for favorites

## Blockers
- None.

## Topic Files
- `ai/specs/tools-and-modes.md` — Permission modes spec (authoritative for tk-k4hv)
- `ai/research/approval-ux-survey-2026-03-30.md` — Competitor approval UX research
- `ai/DESIGN.md` — Architecture and event flow
- `ai/DECISIONS.md` — Decision log
