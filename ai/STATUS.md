# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** P1 native core-loop stabilization  
**Focus:** Review and harden the native Ion TUI/print CLI -> CantoBackend -> Canto agent/session -> provider loop.  
**Active blocker:** `tk-s6p4` — core loop design/refactor and deterministic/live smoke matrix.  
**Queue hygiene:** `tk-mmcs` keeps Pi/Codex/Claude parity planning aligned; `tk-xrgc` keeps `ai/` readable.
**Updated:** 2026-04-28

## Current Truth

- Feature work is frozen behind the native core loop gate. ACP, sandbox polish, approvals polish, privacy expansion, thinking expansion, skills, routing, branching, and other P2/P3 work stay deferred unless they directly block core-loop testing.
- Ion is running with `features.CoreLoopOnly` while `tk-s6p4` is active. Advanced surfaces are hidden or blocked so the P1 loop can be debugged without unrelated prompt/session mutation.
- Canto owns provider-visible transcript, effective history, agent/tool execution, retry, queueing, terminal events, and compaction primitives.
- Ion owns input classification, TUI/CLI lifecycle, display projection, local status/error rows, trust/mode UX, and provider/config selection.
- The detailed reviewed/refactored/pending matrix is tracked in [review/core-loop-review-tracker-2026-04-28.md](review/core-loop-review-tracker-2026-04-28.md). Do not duplicate that matrix here.

## Verified Recently

- Canto-side invalid assistant projection/write validation has been imported into Ion.
- Ion storage rejects non-empty model-visible user/assistant/tool appends; Canto is the single provider-history writer.
- Replay/live transcript rendering share spacing and routine successful tool output compacts as a UI transform only.
- Print CLI preflight and settlement are covered enough to act as the automation surface.
- Startup/resume/continue selection now has real-store coverage for fresh lazy startup, invalid-provider startup, and explicit resume when the active provider config is invalid.
- Slash commands now stay local during active turns instead of being queued as model follow-up prompts.
- Provider-history shape coverage now asserts display-only Ion events are excluded from provider requests and resumed tool results follow matching assistant tool calls.
- Deterministic tests and race-focused backend tests have covered several previously broken terminal, cancel, duplicate-watch, and follow-up-turn paths.
- Live validation is currently provider/environment limited: Fedora is off and OpenRouter free/DeepSeek paths have hit provider/account limits. Deterministic tests remain the active proof path until a live provider is intentionally available.

## Next Action

Continue `tk-s6p4` from the tracker, in this order:

1. Run race-focused checks for the completed native-loop slices.
2. Reassess whether `tk-s6p4` can close or needs a final live smoke when a funded/local provider is available.

## Active Tasks

- `tk-s6p4` — P1 native core loop reliability and smoke matrix.
- `tk-mmcs` — P1 core parity plan and task queue hygiene.
- `tk-xrgc` — P3 AI context dedupe/reorganization; active only because stale docs were blocking agent focus.

Everything else is downstream of the solo native loop.
