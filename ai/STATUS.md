# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** I4 advanced integrations  
**Focus:** ACP headless-agent planning/implementation  
**Active umbrella:** none - `tk-mmcs` is closed  
**Active task:** none - next ready P3 item is `tk-st4q` ACP headless agent  
**Updated:** 2026-05-02

## Current Truth

- Ion has one native baseline path. There is no global stabilization mode.
- I0-I3 are complete: dirty baseline, native boundary refactor, shell polish,
  and safety/trust/sandbox table stakes are green.
- Canto owns durable events, provider-visible history, agent/tool lifecycle,
  reasoning capability translation, and compaction primitives.
- Ion owns TUI/CLI UX, commands, settings/state, product tools, provider
  selection, display projection, safety/trust policy, and session bundle UX.
- Current default tool surface is `bash`, `read`, `write`, `edit`,
  `multi_edit`, `list`, `grep`, and `glob`; `verify` is not registered by
  default.
- Canto is closed unless Ion evidence proves a framework-owned defect.

## Latest Evidence

- `tk-4lty` is closed. It adds a storage-level portable bundle plus CLI surface:
  `--export-session <file>` and `--import-session <file>`.
- The bundle format is versioned JSON and includes Ion `session_meta`, Canto
  event envelopes, Canto ancestry metadata, per-session event checksums, a
  whole-bundle checksum, and explicit conflict detection.
- Canto `7dec159` exposes portable event JSON helpers and ancestry import
  primitives; Ion imports that revision.
- Latest bundle gates passed:
  `go test ./cmd/ion ./internal/storage -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, and
  `go test -race ./cmd/ion ./internal/app ./internal/backend/canto ./internal/backend/canto/tools ./internal/storage -count=1 -timeout 300s`.
- The bundle CLI smoke runs Ion's `main()` path in a subprocess with temp homes:
  export via `--resume <id> --export-session <file>`, import via
  `--import-session <file>`, then verify the imported transcript through
  storage.
- `tk-gopd` is closed. `Ctrl+X` opens the composer buffer in `$VISUAL`,
  `$EDITOR`, or `vi` through Bubble Tea `ExecProcess`, then reloads edited
  content into the composer. It is blocked while turns, approvals, or
  compaction are active.
- Latest editor-handoff gates passed:
  `go test ./internal/app -count=1 -timeout 180s`,
  `go test ./... -count=1 -timeout 300s`, the native race subset, and a tmux
  smoke that rewrote composer text from `draft` to `edited` through the
  external editor path.
- Recent I4 completed slices: portable session bundles, boundary-step
  steering, typed thinking capability filtering, Canto coding primitive audit,
  local `/skills` browser, local `/fork`, `/tree`, and external editor handoff.

## Next Action

1. Continue the I4 queue with `tk-st4q`: ACP headless agent mode.
2. Keep one green slice per commit and avoid reopening Canto unless Ion tests
   expose a framework defect.
