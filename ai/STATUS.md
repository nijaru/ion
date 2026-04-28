# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** P1 native core-loop gate passed; moving to table-stakes parity  
**Focus:** Pi/Codex-style CLI automation and session UX gaps without reopening deferred P2/P3 feature work.  
**Active blocker:** `tk-mmcs` — core parity plan and task queue hygiene.  
**Queue hygiene:** `tk-mmcs` keeps Pi/Codex/Claude parity planning aligned; `tk-xrgc` keeps `ai/` readable.
**Updated:** 2026-04-28

## Current Truth

- The native core-loop gate passed on 2026-04-28. ACP, sandbox polish, approvals polish, privacy expansion, thinking expansion, skills, routing, branching, and other P2/P3 work stay deferred until table-stakes CLI/session UX is intentionally reopened.
- Ion is still running with `features.CoreLoopOnly`, but `/compact` is intentionally reopened as a table-stakes reliability command. ACP, memory commands, MCP registration, rewind/checkpoint polish, privacy expansion, subagents, routing, and other advanced surfaces stay gated.
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
- The native backend/CLI core loop is materially stable by deterministic, race, manual TUI replay, and Fedora live-smoke evidence. `tk-s6p4` is closed.
- TUI shell spacing now keeps a blank row between already-printed transcript/replay rows and the live progress/composer shell. Focused app tests and `go test ./... -count=1` are green after the patch.
- The direct `/provider <name>` command now clears stale progress errors the same way provider-picker selection already did, so old model-listing/provider errors do not remain visible after a provider state change.
- Manual `go run ./... --continue` replay now shows the expected header/resumed-marker/transcript/progress ordering and spacing; terminal control artifacts in the harness remain non-product noise.
- Final gate bundle passed: `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools -count=1`, Fedora endpoint discovery for `qwen3.6:27b`, and live smoke against `local-api` / `qwen3.6:27b`.
- CLI parity started under `tk-mmcs`: `-c` now aliases `--continue`, and `-r` now aliases `--resume` with the same picker/print-mode rules as the long form. Focused CLI tests and `go test ./... -count=1` are green.
- Scriptable model selection now has non-persistent `--model`/`-m` and `--thinking` overrides. Full tests and a live Fedora/local-api print smoke with explicit provider/model/thinking flags passed.
- `/compact` is available again while `CoreLoopOnly` remains on. Focused app coverage and `go test ./... -count=1` pass after reopening it.
- CLI session/error handling is tighter: invalid `--output` values fail before runtime/model execution, and explicit `--continue`/`-c` now errors when no conversation session exists instead of silently starting fresh. Full tests and Fedora text/JSON/continue print smokes pass.
- CLI tool automation now works in untrusted workspaces when the user explicitly asks for it: print-mode `--yolo` / `--mode auto` stays auto for that invocation, while interactive TUI startup still downgrades untrusted workspaces to READ. Fedora JSON tool smoke returned `tool_calls=["bash"]` and `response="done"`.
- Preferred live-smoke order: use Fedora local-api first when available (`http://fedora:8080/v1`, `qwen3.6:27b`). If Fedora is down and live model evidence is needed, use OpenRouter with `deepseek/deepseek-v4-flash` for cheap smoke or `deepseek/deepseek-v4-pro` only when the heavier model is useful.

## Next Action

Continue `tk-mmcs` as the parity/table-stakes track:

1. Continue auditing the CLI surface against Pi/Codex conventions: exit codes, resume/continue print behavior, and JSON shape.
2. Keep `CoreLoopOnly` on while reopening only table-stakes reliability/session surfaces such as compaction.
3. Keep ACP, privacy, subagents, skills, routing, and advanced thinking behind explicit later tasks.

Do not run another broad `ai/` pass by default. The next work is source review and targeted docs only when the code review exposes a design question.

## Active Tasks

- `tk-mmcs` — P1 core parity plan and task queue hygiene.
- `tk-xrgc` — P3 AI context dedupe/reorganization; active only because stale docs were blocking agent focus.

Everything else is downstream of the solo native loop.
