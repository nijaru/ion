# Status: ion (Go)

Fast, lightweight terminal coding agent.

## Current Focus
Two near-term tracks:

- `tk-f1cn` — Canto/Ion Architectural Alignment: PolicyEngine Migration
- `tk-7kga` — Stabilize inline agent loop and TUI
- Everything else is downstream of the solo agent core.

Provider work to keep in mind:
- most providers remain API-key or custom-endpoint integrations
- subscription/OAuth providers need explicit treatment
- `tk-a4m1` exists only as a later evaluation track for ChatGPT subscription support

Deferred idea to remember:
- `tk-hase` — auto thinking budget mode, analogous to primary/fast model selection

Design rule:
- v0.0.0 has no compatibility debt; current bindings and config shapes are allowed to change directly if the end-state is better.

## Next Steps
1. **`tk-f1cn`** — Canto/Ion Architectural Alignment: PolicyEngine Migration
2. **`tk-7kga`** — Stabilize inline agent loop and TUI
3. **`tk-4ft8`** — Canto Context Governor
4. **`tk-di6d`** — Model selector: provider/model tabs and favorites-at-top layout
5. **`tk-4ywr`** — Sessions: lightweight titles and summaries
6. **`tk-5vrj`** — Subagents: runtime semantics and lifecycle
7. **`tk-arhu`** — Subagents: inline Plane B presentation
8. **`tk-pwsl`** — Swarm mode: alternate-screen operator view

## Completed (Recent)
- [x] **Modularize Ion TUI (`tk-2b79`)** — Componentized `internal/app/model.go` into `Viewport`, `Input`, `Broker`, `Picker`, and `Progress`.
- [x] **Approval UX overhaul (`tk-k4hv`)** — Redesigned 3-mode system (READ/EDIT/YOLO) and category-scoped auto-approval ("Always" key) implemented.
- [x] **Architecture Review** — SOTA alignment with Claude Code and Pi-mono. Defined Layer 3 (Canto) and Layer 4 (Ion) boundaries.
- [x] **Pi + Claude guardrails (`tk-y8aj`)** — Captured the ion-actionable patterns and the non-goals so future redesigns stay Go/Bubble Tea idiomatic.
- [x] **TUI hotkeys and model presets (`tk-y64w`)** — Primary/fast toggle, explicit `/primary` and `/fast` commands, and fast/summary config slots wired through the TUI.
- [x] **TUI model selector favorites (`tk-k8g4`)** — Initial model-selector scope-tab pass was superseded by the current provider/model picker with favorites at the top.
- [x] **Approval UX Spec** — 3-mode design (READ/EDIT/YOLO) documented.
- [x] **Agent Compaction Tool (`tk-pw3s`)** — `compact` tool implemented in Ion.
- [x] **RPC/print mode (`tk-r1wx`)** — One-shot query mode and JSONL-friendly scripting surface implemented.
- [x] **Sandbox support (`tk-8s0h`)** — Opt-in bash sandbox planning added with `off`/`auto`/`seatbelt`/`bubblewrap` modes.
- [x] **Retry behavior (`tk-kz3k`)** — Native providers retry transient generation and streaming errors automatically before surfacing a final failure.
- [x] **Swarm/UI design note (`tk-vcmo`)** — Documented inline-vs-swarm boundary, subagent transcript rules, and lightweight session title/summary direction.
- [x] **Planning refresh (`tk-gm5a`)** — Synced `ai/` docs and `tk` priorities to the inline-first, subagents-later dependency order.
- [x] **Canto Migration Complete** — Pinned to `github.com/nijaru/canto`.

## Active Tasks
- `tk-f1cn` [p2 active] Canto/Ion Architectural Alignment: PolicyEngine Migration
- `tk-8s0h` [p2 done] Sandbox support
- `tk-7kga` [p2 active] Stabilize inline agent loop and TUI
- `tk-4ft8` [p2 active] Canto Context Governor (overflow recovery wired; proactive threshold trigger still pending if needed)
- `tk-4ywr` [p3 open] Sessions: lightweight titles and summaries (storage + picker wiring done; terminal title later if needed)
- `tk-5vrj` [p3 open] Subagents: runtime semantics and lifecycle
- `tk-kz3k` [p2 done] Runtime: define retry behavior for provider errors
- `tk-di6d` [p3 open] Model selector: provider/model tabs and favorites-at-top layout
- `tk-arhu` [p4 open] Subagents: inline Plane B presentation
- `tk-pwsl` [p4 open] Swarm mode: alternate-screen operator view

## Pi Cross-Pollination

Research complete (`ai/research/pi-architecture.md`, `ai/design/cross-pollination.md`). 32 insights catalogued with priority matrix.

### Approved for ion (direct implementation)
| Item | Effort | Status | Notes |
|------|--------|--------|-------|
| Bounding-box diff rendering | Medium | ~~Cancelled~~ | BT v2 already handles rendering well; do not treat as active roadmap work |
| Steering/follow-up input queuing | Low | ~~Done~~ | Multi-turn queue, escape-to-pop, visual indicator |
| Paste markers in composer | Low | ~~Done~~ | Large pastes collapsed to markers, expanded on submit |
| RPC/print mode | Medium | ~~Done~~ | --print flag with --prompt or stdin pipe |

### Approved for canto (push upstream)
| Item | Effort | Notes |
|------|--------|-------|
| API-type registry + compat layer | Medium | Reduces new-provider boilerplate |
| Structured compaction + iterative updates | Medium | Better summary quality |
| Three-trigger compaction + overflow recovery | Low | More robust context management |
| Session tree with leaf pointer | Medium | Enables in-place branching |
| Cross-provider message transform | Low | Needed for handoff between providers |
| Faux provider for testing | Low | Aligns with no-mocks philosophy |

### Rejected (not worth building)
| Item | Reason |
|------|--------|
| Full extension system / hot-reload | Overkill for ion's scope |
| Pi packages ecosystem | Premature, no demand |
| Cursor markers for IME | Bubble Tea handles it |
| Configuration cascade | ion's config is simple enough |
| Overlay compositing with style inheritance | Nice-to-have, not needed yet |

## Blockers
- None.

## Topic Files
- `ai/specs/tools-and-modes.md` — Permission modes spec (authoritative for tk-k4hv)
- `ai/specs/swarm-mode-and-inline-subagents.md` — Inline subagent rendering, future swarm mode, and session title/summary direction
- `ai/research/approval-ux-survey-2026-03-30.md` — Competitor approval UX research
- `ai/research/pi-architecture.md` — Pi-mono architecture analysis
- `ai/research/ion-architecture.md` — Ion architecture analysis
- `ai/design/cross-pollination.md` — Pi → canto/ion actionable insights
- `ai/DESIGN.md` — Architecture and event flow
- `ai/DECISIONS.md` — Decision log
