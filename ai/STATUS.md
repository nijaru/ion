# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** I4 advanced integrations
**Focus:** Reopen deferred integrations from the stable I0-I3 baseline.
**Active umbrella:** none - `tk-mmcs` is closed.
**Active task:** none - choose the next I4 task.
**Updated:** 2026-05-02

## Current Truth

- Ion has one native baseline path. The old global stabilization split is gone.
- Current P1 tools are `bash`, `read`, `write`, `edit`, `multi_edit`, `list`,
  `grep`, and `glob`; `verify` is not registered by default.
- Canto owns provider-visible history, durable events, turn execution, retry,
  cancellation settlement, and compaction primitives.
- Ion owns TUI/CLI UX, commands, settings/state, display projection, product
  tools, provider selection, and safety/trust policy.
- Canto is clean and remains framework-focused. Ion is the active repo unless a
  test or smoke proves a Canto-owned defect.

## Latest Evidence

- Source/docs dirty baseline committed as `a571cef`:
  `feat(stabilization): close native shell baseline`.
- Gates before that commit passed:
  `go test ./... -count=1 -timeout 300s` and the native race subset.
- A concrete backend watcher bug was fixed during closeout:
  `TestCrossProviderHandoffPreservesPromptTruth` previously timed out because a
  `translateEvents` goroutine could wait forever on an unstopped watch. Focused
  backend tests and the full suite now pass.
- Prompt-budget and tool-surface research have been distilled into specs and
  decisions. Research files remain evidence, not canonical behavior.
- AI context cleanup is committed in Ion and Canto. Ion root `ai/` now uses the
  five canonical files, and Canto feedback intake is folded into its tracker.
- App shell boundary refactor is complete: command catalog, settings,
  session/cost, rewind, picker, and runtime switching helpers are split out of
  `internal/app/commands.go`.
- Latest app-shell gates passed:
  `go test ./internal/app -count=1 -timeout 120s`,
  `go test ./... -count=1 -timeout 300s`, and
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
- CLI/config boundary refactor is complete: flag registration and normalization
  live in `cmd/ion/flags.go`, runtime selection in `cmd/ion/selection.go`, and
  backend/session opening in `cmd/ion/runtime.go`.
- Latest CLI/config gates passed:
  `go test ./cmd/ion ./internal/config -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and the native race subset.
- File tools are split by responsibility: common workspace/checkpoint helpers
  remain in `file.go`, with read/write/edit/list implementations in focused
  files.
- Latest file-tool gates passed:
  `go test ./internal/backend/canto/tools -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and the native race subset.
- CantoBackend lifecycle responsibilities are split out of the previous
  monolithic `internal/backend/canto/backend.go`: metadata, lifecycle,
  providers/retry, turns/cancel, event translation, compaction, policy, memory,
  reasoning, and deferred surfaces now live in focused package files.
- Latest backend-lifecycle gates passed:
  `go test ./internal/backend/canto -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and the native race subset.
- Markdown rendering now preserves GFM task checkboxes, autolinks,
  strikethrough content, and inline code/autolinks inside rendered tables.
- Latest markdown gates passed:
  `go test ./internal/app -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and the native race subset.
- Edit-surface design is distilled into `ai/specs/tools-and-modes.md` and
  `ai/DECISIONS.md`: keep `write`, `edit`, and `multi_edit` through I2; defer a
  Pi-style merged `edit(edits[])` until there is eval evidence.
- I2 final shell sweep passed against Fedora `local-api/qwen3.6:27b-uncensored`:
  live request-history smoke verified tool call, persisted resume, and follow-up
  provider history; tmux text capture covered fresh launch, `/tools`,
  `/settings`, `/session`, queued input, bash display, `--continue`, and resumed
  follow-up.
- Read mode now hides unavailable write/execute tools from provider-visible
  tool specs and from `ToolSurface()` summaries while policy still denies any
  direct write/execute call that reaches execution.
- Latest read-mode gates passed:
  `go test ./cmd/ion ./internal/backend ./internal/backend/canto -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, the native race subset, and a tmux
  text capture for `--mode read` startup plus `/tools`.
- Workspace trust gating is restored for I3: `workspace_trust=prompt|strict`
  starts unknown workspaces in READ mode, `/trust` is available for prompt-mode
  trust, strict mode stays externally managed, and `workspace_trust=off`
  disables the gate.
- Latest trust-gate gates passed:
  `go test ./cmd/ion ./internal/app ./internal/workspace -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, the native race subset, and a tmux
  text capture for unknown-workspace startup, blocked `/mode auto`, `/trust`,
  and post-trust `/tools`.
- Sandbox posture is now cached in app state and shown in startup, `/tools`, and
  the footer/status line. Deferred feature copy no longer references native-loop
  stabilization as an active phase.
- Latest sandbox/status gates passed:
  `go test ./cmd/ion ./internal/app ./internal/backend/canto -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, the native race subset, and a tmux
  text capture with `ION_SANDBOX=auto`.
- `tk-mmcs` is closed. Final Fedora `local-api/qwen3.6:27b-uncensored` live
  smoke passed with a real bash tool call, persisted resume, follow-up returning
  `continued`, and provider request capture verifying prior user, tool call,
  tool result, assistant, and resume history order.
- ACP agent stderr is separated from the user-visible session stream. It is
  suppressed by default and can be redirected to a debug file with
  `ION_ACP_STDERR_LOG`.
- Latest ACP stderr gates passed:
  `go test ./internal/backend/acp -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and the native race subset.
- ACP `session/new` now sends an explicit Ion `_meta.ion` context payload with
  cwd, branch, model, Ion session id, resume hint, and project instruction text
  when present; the request also sends ACP's required explicit empty
  `mcpServers` list.
- Latest ACP context gates passed:
  `go test ./internal/backend/acp -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, the native race subset, and
  `go test -race ./internal/backend/acp -count=1 -timeout 180s`.
- ACP token usage extension metadata now maps to `session.TokenUsage` from
  `_meta.tokenUsage`, `_meta.token_usage`, `_meta._tokenUsage`, and
  `_meta.usage` payloads on session notifications or update payloads.
- Latest ACP token-usage gates passed:
  `go test ./internal/backend/acp -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, the native race subset, and
  `go test -race ./internal/backend/acp -count=1 -timeout 180s`.

## Next Action

1. Commit the completed `tk-6zy3` slice.
2. Choose the next I4 task from `tk ready`; current highest-priority options are
   merged edit evaluation, background bash monitor workflow, subagent context
   forking, and boundary-step steering.
3. Keep native Canto/Ion loop behavior as the acceptance baseline while adding
   advanced integrations.
4. Continue one green slice per commit.

## Active Tasks

- none.
