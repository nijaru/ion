# Architecture Boundary Audit (2026-03-27)

## Answer

ion is reasonably well separated internally, but it is not yet a clean reusable external agent library.

Current state:
- `internal/app/` is a Bubble Tea host, not the agent core.
- `internal/backend/` and `internal/session/` form the runtime boundary the TUI consumes.
- another host inside this repo could reuse that runtime boundary with moderate effort.
- an external product cannot reuse it cleanly today because the boundary is still `internal/` and still depends on ion-owned config/storage/session types.

Recommended direction:
- prefer a headless `ion --agent` mode before trying to export a public runtime package
- keep the runtime/TUI split, but reduce `cmd/ion/main.go` orchestration ownership so the headless and TUI entrypoints can share one bootstrap path

## Evidence

### What is already good

1. `internal/app/` consumes interfaces instead of canto internals directly.
2. `internal/session.AgentSession` is already the event-driven host/runtime seam.
3. `internal/backend.Backend` hides native vs ACP implementation differences from the TUI.
4. the startup/scrollback logic is in `cmd/ion/main.go` and `internal/app/`, not mixed into the runtime adapters.

### Where the boundary is still too ion-shaped

1. `cmd/ion/main.go` owns too much runtime bootstrap:
   - config resolution
   - provider routing
   - storage open/resume
   - session metadata sync
   - startup transcript rendering
   - runtime switcher construction

2. `internal/backend.Backend` still exposes host mutation hooks:
   - `SetStore(storage.Store)`
   - `SetSession(storage.Session)`
   - `SetConfig(*config.Config)`

   These make the runtime easy to wire from ion, but they are not a minimal host API.

3. `internal/session.AgentSession.Meta()` returns a loose `map[string]string`.
   - practical for now
   - weakly typed for a long-term reusable boundary

4. everything is still under `internal/`.
   - good for local refactors
   - prevents external reuse by design

5. `internal/backend/native/` still exists as an empty stale directory.
   - harmless
   - misleading during architecture review

## Reuse assessment

### Could another host inside this repo reuse the agent core?

Yes.

A second host could build on:
- `internal/backend.Backend`
- `internal/session.AgentSession`
- `internal/storage.Store`

without needing Bubble Tea.

### Could another external product build on ion as a library today?

Not cleanly.

Reasons:
- the packages are `internal/`
- bootstrap is CLI-app-shaped
- backend/session contracts are still coupled to ion-owned storage/config types

## Recommended refactors

### 1. Shared runtime bootstrap

Extract a hostable constructor from `cmd/ion/main.go` that returns:
- backend
- storage session
- startup transcript lines
- startup transcript entries
- runtime switcher

That would let:
- the TUI path
- a future `ion --agent` headless path

share one runtime-open flow.

### 2. Tighten the backend boundary

Replace setter-style initialization over time with a constructor-style runtime host API.

Goal:
- fewer mutable post-construction hooks
- clearer required dependencies

### 3. Decide reuse strategy explicitly

Do not drift into both approaches at once.

Choose between:
- exported reusable package
- headless `ion --agent`

Recommendation:
- do `ion --agent` first
- revisit exported packages only if a second real host needs direct in-process reuse

### 4. Remove stale directory noise

Delete or document:
- `internal/backend/native/`

## Non-goals

- no public package extraction yet
- no ACP-agent implementation in this audit
- no TUI refactor here

## Conclusion

ion already has a real internal runtime/TUI separation.

The next architectural improvement is not “rewrite the boundary.” It is:
- centralize runtime bootstrap
- choose headless mode vs exported library deliberately
- stop leaving stale directories and app-owned mutation hooks ambiguous
