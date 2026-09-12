# Core storage engine review — 2026-09-12

Scope: Ion core session/runtime persistence only. This review intentionally excludes long-term knowledge/memory, shared task-board systems, vector stores, and other post-core experiments.

## Workload Ion is designing for

Ion's canonical session storage has unusual but favorable properties:

- local-first embedded operation;
- one semantic mutation owner per session;
- short write transactions only;
- provider/tool/process/network work always outside transactions;
- many concurrent external effects may settle through the serialized writer;
- many readers/observers may exist;
- root agent and retained workers inside one cooperating group may require atomic cross-agent transitions;
- independent top-level sessions do not require cross-session atomicity and may run concurrently;
- crash recovery, cancellation fencing and exact effect intent/settlement matter more than raw transaction throughput.

The engine should be judged against this workload rather than generic "AI agent" concurrency claims.

## Candidate boundaries

### One root-wide database

Advantages:

- simplest discovery;
- one schema/version location;
- cross-session queries are trivial.

Costs:

- unrelated sessions share one SQLite writer-lock/WAL/checkpoint domain;
- corruption/backup/migration/archive affect the whole root;
- physical contention exists where no semantic atomicity is required.

Keep as the current implementation baseline for P2 measurements, not the preferred target.

### One database per agent

Rejected as the core boundary.

Agents in one group need atomic operations across agent identity, conversation creation, messages, supervision/cancellation barriers, task dependencies, authority and resource accounting. Splitting those records across agent databases would force explicit cross-database protocols into normal group operation.

### One database per top-level session/group

Leading design.

Root agent and retained workers that need transactional coordination share one `session.sqlite`. Independent top-level sessions have independent stores and owners.

This aligns physical isolation with semantic ownership:

```text
session A owner -> session A DB/WAL
session B owner -> session B DB/WAL
session C owner -> session C DB/WAL
```

Within a session, concurrency comes from asynchronous effect execution outside mutation authority, not competing canonical writers.

## SQLite

Primary sources:

- https://www.sqlite.org/wal.html
- https://www.sqlite.org/whentouse.html
- https://www.sqlite.org/lang_attach.html
- https://www.sqlite.org/backup.html

Relevant properties:

- WAL allows readers and a writer to proceed concurrently but one database file has one writer at a time.
- This is compatible with Ion because one semantic writer per session is intentional and write transactions are short.
- SQLite documents that transactions across multiple attached databases are not crash-atomic as a set when the main database uses WAL. Therefore Ion should not split one session transaction across category-specific WAL databases.
- SQLite provides mature transaction/crash semantics and supported live-backup mechanisms.

Recommendation: keep SQLite as the default production engine while P1/P2 establish the new core.

## Turso Database

Primary sources:

- https://github.com/tursodatabase/turso
- https://turso.tech/blog/concurrent-writes-in-practice
- https://turso.tech/blog/concurrent-writes-on-turso-cloud

Current relevant properties at review time:

- ground-up Rust database compatible with SQLite's SQL/file/C-API direction;
- not yet 1.0 and compatibility is not yet complete;
- supports MVCC/`BEGIN CONCURRENT` for concurrent writers;
- native async/I/O work, CDC and sync capabilities are strategic goals/features;
- concurrent-write features are still comparatively new and some deployment surfaces are described as preview/experimental.

### Why concurrent writes do not change Ion's core design

Ion's one-writer rule is semantic, not an accidental workaround for SQLite.

Multiple concurrent canonical writers would still require Ion to resolve:

- command ordering;
- duplicate admission;
- authority/revision races;
- cancellation versus settlement;
- successor ownership;
- observation ordering;
- external-effect fencing.

MVCC would move some conflicts to commit/retry rather than remove those semantic conflicts. A deterministic session command line remains simpler to reason about and test.

### When Turso could become worthwhile

P2 should consider an isolated equivalent-schema benchmark if representative data shows one of these:

- SQLite commit/lock/checkpoint overhead becomes material even with per-session partitioning and short transactions;
- native async storage materially improves runtime behavior versus a dedicated blocking storage worker;
- a concrete accepted local/remote sync requirement appears;
- another Turso feature solves a measured core problem without weakening the session ownership model.

Do not add Turso as a production dependency merely for optionality. Keep the store interface private and avoid public APIs that expose SQLite-specific representation, so a later engine comparison remains feasible.

## libSQL

Primary source:

- https://github.com/tursodatabase/libsql

libSQL adds embedded replicas/remote access but retains SQLite's fundamental single-writer model. Those remote/replica features are not current Ion core requirements. Turso's own project documentation now positions the newer Turso Database as the forward development direction.

Recommendation: no core dependency or dedicated P2 track unless a concrete remote-replica requirement emerges.

## Client/server databases

Postgres or another server engine becomes relevant if Ion later accepts a multi-host or multi-user writable-session product requirement. Introducing a server solely to get write concurrency is not justified for the local-first core.

## Artifacts

Large opaque outputs should remain separate from the database when this reduces row/WAL churn. The database owns durable reference/integrity metadata. Publication ordering must prefer reclaimable orphan files over committed references to missing content.

## Discovery catalog

A global `catalog.sqlite` is optional derived state, not part of session correctness. Start with directory/session-store discovery if adequate. Add a rebuildable catalog only when listing/opening scale justifies it.

## Current recommendation

```text
Ion data root/
  sessions/
    <SessionId>/
      session.sqlite      # canonical root + retained-worker state
      artifacts/          # large retained output/evidence

  catalog.sqlite          # optional rebuildable cache, only if needed
```

- SQLite is the baseline engine.
- One canonical semantic writer exists per session.
- Different sessions may execute/write independently.
- No per-agent databases.
- No category-specific databases inside one session transaction.
- No knowledge/memory/vector/task-board storage in the core design.
- Turso is monitored and, if justified by P2 measurements, benchmarked against the same representative workload.

This recommendation remains evidence-gated. P2 must compare the current root-wide database against per-session stores and measure real lock/WAL/RSS/query/backup behavior before the physical migration is declared final.
