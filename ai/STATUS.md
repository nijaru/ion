# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** P1 native core-loop gate passed; moving to table-stakes parity  
**Focus:** Pi/Codex-style CLI automation and session UX gaps without reopening deferred P2/P3 feature work.  
**Active blocker:** `tk-mmcs` — core parity plan and task queue hygiene.  
**Queue hygiene:** `tk-mmcs` keeps Pi/Codex/Claude parity planning aligned; deferred ACP/P3/P4 tasks are blocked behind it so `tk ready` stays focused.
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
- Startup header readability improved: the trust notice now says `Workspace: not trusted. READ mode active. Run /trust to enable edits.`, tool metadata is labeled as `Tools: N registered`, and startup metadata uses readable gray/warning/ok color rather than faint-only styling.
- `/help` readability improved: help output now starts on its own separated block, section headers stay highlighted, command/key labels are styled separately, and descriptions remain normal contrast.
- Tab completion now covers both slash commands and current-token `@file` references. File completion stays workspace-bound, completes directories with a trailing slash, files with a trailing space, and rejects `..` escapes.
- Session browser polish started: `/resume` rows now show session count, a search hint, stable title/preview labels, and useful preview/model/branch/age metadata without bogus ages for missing timestamps.
- Session UX now includes a Pi-like `/session` command that reports durable session id, provider/model, mode, branch, message counts, token totals, and cost without sending a model turn. It reports `id: none` for lazy pre-turn sessions and has regression coverage that it does not materialize a session row. Focused app command tests and `go test ./... -count=1 -timeout 120s` are green.
- Config/session state hygiene pass is active under `tk-594o`. First bug fixed: `/settings` writes stable config, then reloads merged runtime config before updating the active backend, so `state.toml` provider/model selections remain active and do not leak into `config.toml`. Focused settings tests and `go test ./... -count=1 -timeout 120s` are green.
- Provider override scoping is tightened: custom `auth_env_var` and `extra_headers` now apply only to providers that support custom endpoints, so local/custom endpoint settings do not leak into OpenRouter/default-provider runtime config. Provider/backend focused tests and `go test ./... -count=1 -timeout 120s` are green.
- Thinking state persistence is narrowed: `/thinking` and the thinking picker update only reasoning-effort fields in `state.toml`, preserving existing selected provider/model without freezing config defaults into mutable state. Focused app/config tests and `go test ./... -count=1 -timeout 120s` are green.
- Startup config error copy now matches the current command surface: no-provider/no-model errors point to `/provider`, `/model`, env vars, and CLI flags instead of stale `Ctrl+P`/`Ctrl+M` picker guidance. Focused cmd tests and `go test ./... -count=1 -timeout 120s` are green.
- Startup resume/continue now uses stored session provider/model metadata by default, matching `/resume`; explicit `--provider`/`--model` still override. Focused startup metadata tests, `go test ./... -count=1 -timeout 120s`, and OpenRouter Minimax free print smoke are green.
- Preferred live-smoke order: use Fedora local-api first when available (`http://fedora:8080/v1`, `qwen3.6:27b`). Fedora is temporarily down by user request; while it is down, use OpenRouter cheap/free models for live checks: `minimax/minimax-m2.5:free` when available, `deepseek/deepseek-v4-flash` for cheap checks, or `deepseek/deepseek-v4-pro` only when a stronger separate-provider check is useful.
- Current OpenRouter fallback evidence: `minimax/minimax-m2.5:free` returned `ok` with a 120s print-mode timeout; a 45s Minimax attempt timed out, and `deepseek/deepseek-v4-flash` returned OpenRouter `402 Payment Required`.

## Next Action

Continue `tk-mmcs` as the parity/table-stakes track:

1. Continue `tk-594o` source review for provider/model/session state ownership, startup persistence behavior, and transcript inspection/config commands.
2. Keep `CoreLoopOnly` on while reopening only table-stakes reliability/session surfaces such as compaction.
3. Keep ACP, privacy, subagents, skills, routing, and advanced thinking blocked behind `tk-mmcs`.

Do not run another broad `ai/` pass by default. The next work is source review and targeted docs only when the code review exposes a design question.

## Active Tasks

- `tk-mmcs` — P1 core parity plan and task queue hygiene.
- `tk-594o` — P2 config/session state hygiene review.
- `tk-xrgc` — P3 AI context dedupe/reorganization; active only because stale docs were blocking agent focus.

Everything else is downstream of the solo native loop.
