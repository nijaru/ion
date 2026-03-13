# ion Status

## Current State

| Metric | Value | Updated |
| --- | --- | --- |
| Phase | Active rewrite: build the Go host for real and judge Bubble Tea v2 by execution, not theory | 2026-03-12 |
| Status | `codex/go-rewrite-host` is the active implementation branch | 2026-03-12 |
| Active branch | `codex/go-rewrite-host` | 2026-03-12 |
| Go host | `go-host/` now has real app/backend/session package boundaries, streamed backend events, and app-level tests | 2026-03-12 |
| Rust TUI | `tui-work` remains the fallback reference branch, but is no longer the active direction | 2026-03-12 |

## Active Work

1. `tk-3bd5` (p1): build the Go rewrite host for real on `codex/go-rewrite-host`
2. `tk-n6f7` (p1): Bubble Tea v2 decision/research track and architecture notes
3. `tk-nxz3` (p3): ACP architecture for external coding agents
4. `tk-avhl` (p1): Rust TUI parity work remains as reference context on `tui-work`

## Current Findings

- The project is now actively exploring an all-Go rewrite path rather than continuing to default to the custom Rust TUI.
- The current question is no longer whether Bubble Tea v2 is interesting; it is whether it holds up when we build ion's host for real.
- `go-host/` now has:
  - an app package
  - a backend boundary
  - session entry types
  - a transcript viewport
  - a multiline composer
  - footer/status rendering
  - streamed backend events
  - app-level tests for transcript/layout behavior
- Practical finding from the first real runtime pass: Bubble Tea's textarea gives us a stable multiline composer immediately. The remaining work is making it feel like ion, not fighting corruption or panic-level footer bugs.
- The next meaningful checkpoint is a real session/backend boundary in place of the fake backend, shaped so it can later support ACP or a native ion runtime.
- The custom Rust TUI still provides useful lessons, but it is no longer the main implementation bet.

## Next Steps

1. Replace the fake backend with a more realistic session boundary and event model.
2. Improve transcript/composer/footer behavior until the Go host feels like ion rather than a framework sample.
3. Decide how native ion runtime and ACP-backed external agents fit behind the same host boundary.
4. Once the host shape is solid, decide whether the rest of ion should move to Go as well.

## Key References

| Topic | Location |
| --- | --- |
| Active rewrite task | `.tasks/tk-3bd5.json` |
| Bubble Tea research | `.tasks/tk-n6f7.json` |
| Bubble Tea decision note | `ai/research/bubbletea-v2-vs-rust-tui-host-2026-03-12.md` |
| Go rewrite host | `go-host/` |
| ACP architecture tracker | `.tasks/tk-nxz3.json` |
| Rust TUI audit reference | `ai/review/tui-lib-audit-2026-03-11.md` |
