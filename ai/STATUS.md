# Status: ion

Fast, lightweight terminal coding agent.

**Phase:** I4 advanced integrations  
**Focus:** portable sessions and other table-stakes advanced workflow pieces  
**Active umbrella:** none - `tk-mmcs` is closed  
**Active task:** `tk-4lty` - Session: cross-host export/import bundle  
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

- `tk-4lty` now has a storage-level portable bundle plus CLI surface:
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
- Recent I4 completed slices: boundary-step steering, typed thinking
  capability filtering, Canto coding primitive audit, local `/skills` browser,
  local `/fork` and `/tree` session branching.

## Next Action

1. Finish `tk-4lty`: run a real file-level export/import smoke using the CLI,
   update task logs, then close the task if the smoke is clean.
2. Continue the I4 queue with the next ready P3 item:
   `TUI: external editor handoff` or `ACP: Implement ion as an ACP agent`.
3. Keep one green slice per commit and avoid reopening Canto unless Ion tests
   expose a framework defect.
