# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** P1 native core-loop stabilization  
**Focus:** Comprehensive file-by-file audit of the native Ion TUI/print CLI -> CantoBackend -> Canto agent/session -> provider loop.  
**Active blocker:** `tk-s6p4` — core loop design/refactor and deterministic/live smoke matrix.  
**Queue hygiene:** `tk-mmcs` keeps Pi/Codex/Claude parity planning aligned; `tk-xrgc` keeps `ai/` readable.
**Updated:** 2026-04-28

## Current Truth

- Feature work is frozen behind the native core loop gate. ACP, sandbox polish, approvals polish, privacy expansion, thinking expansion, skills, routing, branching, and other P2/P3 work stay deferred unless they directly block core-loop testing.
- Ion is running with `features.CoreLoopOnly` while `tk-s6p4` is active. Advanced surfaces are hidden or blocked so the P1 loop can be debugged without unrelated prompt/session mutation.
- Canto owns provider-visible transcript, effective history, agent/tool execution, retry, queueing, terminal events, and compaction primitives.
- Ion owns input classification, TUI/CLI lifecycle, display projection, local status/error rows, trust/mode UX, and provider/config selection.
- Keep Canto and Ion split, but treat Ion as Canto's acceptance test during stabilization. Canto public-framework expansion is deferred until Ion's native minimal loop is stable.
- The detailed reviewed/refactored/pending matrix is tracked in [review/core-loop-review-tracker-2026-04-28.md](review/core-loop-review-tracker-2026-04-28.md). Do not duplicate that matrix here.

## Recent Evidence, Not Completion

- Canto has two pushed session-history fixes in the active Ion dependency: invalid empty assistant writes are rejected at write boundaries, and raw `LastAssistantMessage` now skips legacy invalid assistant rows during turn finalization.
- Ion has reviewed/patched native-path slices for feature freeze enforcement, CLI startup/resume/print lifecycle, backend event translation, storage/replay projection, app turn lifecycle, transcript replay rendering, core tools, provider startup state, and deferred-surface isolation.
- Deterministic gates are green after importing Canto `d37beda`: focused native package tests plus `go test ./...`.
- Fedora live smoke is green against `local-api` / `qwen3.6:27b`: tool call, approval, persistence, resume, and follow-up turn all completed without provider-history errors.
- New Canto C4/C5 cancellation/tool-result fixes are pushed in Canto `5ce3c1f` and imported into Ion: streaming turns now stop before an extra provider step after cancellation, step terminal events survive canceled contexts, and canceled tool turns persist a matching tool result so provider-visible history is not left with dangling tool calls.
- Ion deterministic gates and Fedora live smoke are green after importing Canto `5ce3c1f`.
- Ion overflow recovery now uses Canto runtime-level recovery instead of provider-level request retry, so context-overflow compaction rebuilds the provider-visible prompt before retrying. Focused overflow coverage, full Ion tests, Fedora live smoke, and Canto prompt/LLM/governor/runtime gates are green after this patch.
- The native backend/CLI core loop is now materially stable by deterministic and live-smoke evidence. `tk-s6p4` should stay open for the next gate: TUI/replay polish and any remaining manual terminal bugs, especially spacing and resumed transcript presentation.
- TUI shell spacing now keeps a blank row between already-printed transcript/replay rows and the live progress/composer shell. Focused app tests and `go test ./... -count=1` are green after the patch.

## Next Action

Continue `tk-s6p4` as a comprehensive audit, not as bug slices:

1. Continue the TUI/replay gate under `tk-s6p4`: stale progress/error presentation, resumed transcript presentation in the actual terminal, and compact routine tool display.
2. Keep backend/CLI regressions covered while fixing the TUI shell; re-run full Ion gates and Fedora live smoke after any native-loop changes.
3. Keep picker/help/provider UX, permissions/trust polish, ACP, privacy, subagents, skills, and routing behind the stable TUI/replay gate unless a bug directly corrupts core state.

Do not run another broad `ai/` pass by default. The next work is source review and targeted docs only when the code review exposes a design question.

## Active Tasks

- `tk-s6p4` — P1 native core loop reliability and smoke matrix.
- `tk-mmcs` — P1 core parity plan and task queue hygiene.
- `tk-xrgc` — P3 AI context dedupe/reorganization; active only because stale docs were blocking agent focus.

Everything else is downstream of the solo native loop.
