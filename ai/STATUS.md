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
- This is meaningful progress, not completion. Canto runtime/agent/tool internals still need the remaining C3-C7 file-by-file review before `tk-s6p4` can close.

## Next Action

Continue `tk-s6p4` as a comprehensive audit, not as bug slices:

1. Finish Canto C3-C7 audit: runtime queue/runner, agent streaming/non-streaming exits, tool execution lifecycle, provider-visible prompt construction, retry/compaction surfaces.
2. Patch any Canto findings upstream first, push, and import the exact revision into Ion.
3. Re-run Ion deterministic gates and Fedora live smoke after each Canto import or native-loop patch.
4. Only after C3-C7 are reviewed should the work shift to the deferred TUI spacing/picker/help polish.

Do not run another broad `ai/` pass by default. The next work is source review and targeted docs only when the code review exposes a design question.

## Active Tasks

- `tk-s6p4` — P1 native core loop reliability and smoke matrix.
- `tk-mmcs` — P1 core parity plan and task queue hygiene.
- `tk-xrgc` — P3 AI context dedupe/reorganization; active only because stale docs were blocking agent focus.

Everything else is downstream of the solo native loop.
