# Canto Upgrade Plan For Ion

**Date:** 2026-03-29

## Result

This migration has been completed in Ion against the GitHub canto module.

- Ion commit: `5b5d06b8` `refactor(ion): finish canto migration and prune deps`
- Canto commit: `9b90838` `refactor(llm): remove catwalk core dependency`
- Verification: `go test ./...` passes in Ion with `github.com/nijaru/canto` resolved from GitHub at the same baseline

The plan below is retained as the original migration checklist.

## Answer First

Update Ion against GitHub `origin/main` canto in four passes:

1. replace removed memory APIs in `internal/storage` and `internal/backend/canto`
2. remove direct `catwalk` usage from provider setup and tests
3. switch session/event watching to `Watch()` subscriptions
4. prune dependencies and remove registry fallback assumptions so Ion owns vendor metadata and picker UX

Do this as a clean rewrite, not a compatibility migration:

- no legacy adapters
- no deprecated paths kept around
- no fallback compatibility code for pre-refactor Canto
- no Catwalk runtime dependency left in Ion when the migration is done

## Pull Target

- Base the migration on GitHub Canto through `9b90838` (`refactor(llm): remove catwalk core dependency`)

## Current Breakages

- This section is historical; the breakages below were resolved during the migration.

## Work Plan

### 1. Memory migration

Files:
- `internal/storage/canto_store.go`
- `internal/storage/storage.go`
- `internal/backend/canto/backend.go`

Changes:
- keep `memory.CoreStore` as the durable substrate
- decide whether Ion should:
  - use `memory.Manager` directly for durable writes/retrieval
  - or wrap `CoreStore.UpsertMemory` / `CoreStore.SearchMemories` itself
- replace `SaveKnowledge` with `UpsertMemory` or `Manager.Write`
- replace `SearchKnowledge` with `SearchMemories` or `Manager.Retrieve`
- choose an Ion namespace convention now:
  - likely `memory.Namespace{Scope: memory.ScopeWorkspace, ID: cwd}`
- choose an Ion role now:
  - likely `memory.RoleSemantic` for current “knowledge” behavior

Recommendation:
- use `memory.Manager` in `internal/backend/canto/backend.go` for tools
- keep `internal/storage` exposing Ion-friendly `KnowledgeItem`, but back it with `memory.Memory`

### 2. Provider migration

Files:
- `internal/backend/canto/backend.go`
- `internal/backend/canto/backend_test.go`

Changes:
- replace `catwalk.Provider` construction with:
  - `llm.ProviderConfig`, or
  - family constructors in `github.com/nijaru/canto/llm/providers`
- prefer family constructors so Ion’s provider switch stays aligned with its own provider catalog
- update test fakes to return `[]llm.Model` instead of `[]catwalk.Model`

### 3. Session/watch migration

Files:
- `internal/backend/canto/backend.go`

Changes:
- replace legacy `runner.Subscribe(...)` path with `runner.Watch(...)`
- hold the returned `Subscription`
- read from `sub.Events()`
- `defer sub.Close()` in the turn lifecycle

Note:
- the temporary `Subscribe` compatibility shape was removed from Ion during the migration; keep Ion aligned to the GitHub canto module surface rather than any local checkout state

### 4. Registry and metadata boundary

Files:
- `internal/backend/registry/models.go`
- `internal/backend/registry/registry.go`

Changes:
- remove Catwalk fallback from Ion runtime-critical paths
- keep Ion-owned provider aliases, endpoint handling, and picker ordering in Ion
- if a live fallback is still needed, move it behind Ion-owned provider metadata fetchers rather than Catwalk

Recommendation:
- keep built-in metadata and direct provider API fetchers
- drop Catwalk as a runtime dependency after the migration

### 5. Dependency pruning

Files:
- `go.mod`
- `go.sum`

Changes:
- remove `charm.land/catwalk` once provider and registry code stop importing it
- run `go mod tidy`
- verify no transient fallback package remains only to support removed code paths

Rule:
- if a dependency only exists for a compatibility bridge, delete the bridge instead of carrying the dependency

## Suggested Commit Order

1. `refactor(storage): migrate Ion knowledge wrappers to canto memory`
2. `refactor(canto): switch providers and watcher APIs off catwalk-era surface`
3. `refactor(registry): remove catwalk model catalog fallback`
4. `build(deps): prune catwalk and stale transitive deps`
5. `test(canto): update backend fakes and migration coverage`

## Verification

- `go test ./...`
- manual smoke:
  - start Ion with a native provider
  - send a turn
  - run at least one tool requiring approval
  - verify session resume
  - verify memory tool registration still works or is intentionally disabled pending UX
