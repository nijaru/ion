# P2 storage-topology prototype evidence

Status: initial structural prototype validated; physical migration and performance decision remain open.

Validated code head: `0c45553538e0244e27cc13ac0719f8afb7d73cbb`.

GitHub Actions run: `34705477866` on Rust 1.98.0.

Required repository gates passed:

- `cargo fmt --check`
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
- `cargo test --locked --workspace`

## Question

The current production store uses one SQLite database for an Ion data root. DESIGN.md revision 2 proposes a different physical boundary: one authoritative SQLite database per session/group, a rebuildable catalog, and separate stores only for state whose ownership/lifecycle is independent.

This prototype asks only whether the proposed boundary has the expected basic correctness properties. It does **not** decide that the topology is faster, cheaper, or ready for production migration.

## Fixture

`crates/ion-core/tests/p2_storage_topology.rs` builds an isolated SQLite fixture with this shape:

```text
root/
  catalog.sqlite
  sessions/
    session-a/session.sqlite
    session-b/session.sqlite
```

Each session file uses WAL, foreign keys, and FULL synchronization in the fixture. The catalog contains discovery metadata only.

## Validated structural properties

### Core transition stays inside one database

A transaction inserts a durable task and then attempts an invalid effect referencing a missing task. The foreign-key failure rolls the transaction back, leaving neither record committed.

Evidence: core state that must change atomically can remain inside one session SQLite transaction.

Non-claim: this test does not yet exercise the complete Ion production schema or every P1 transaction.

### Session writer locks are physically independent

The test holds `BEGIN IMMEDIATE` open in session A. A second writer to the same file is rejected while a writer to session B commits successfully.

Evidence: separate session files give unrelated sessions separate SQLite writer-lock domains.

Non-claim: this is not a throughput benchmark and does not quantify filesystem, WAL-checkpoint, page-cache, or process-level overhead.

### Catalog is reconstructable

The fixture deliberately writes stale catalog metadata, deletes/recreates the catalog, scans complete session stores, reads authoritative session metadata, and reconstructs the correct catalog.

Evidence: discovery metadata does not need to be the source of session truth.

### Session creation can precede catalog publication

A complete session store exists and is discoverable before `catalog.sqlite` exists. Rebuilding the catalog later publishes it.

Evidence: an interrupted catalog publication need not make an otherwise complete session unreachable or corrupt.

## Why the core is not split across several WAL databases

SQLite's documented multi-database atomicity guarantee for `ATTACH` excludes WAL-mode main databases. Ion therefore must not place pieces of one semantic session transition into category-specific WAL files and assume a crash-atomic distributed commit.

The candidate boundary is instead:

```text
one session command
      |
      v
session.sqlite  <-- one atomic authority boundary
      |
      +--> artifact references published safely

independent lifecycle state
      |
      +--> catalog.sqlite / optional knowledge / derived indexes
           synchronized explicitly, never as hidden parts of the core commit
```

## Still required before P2 closes

The physical topology remains a hypothesis until the following are measured or fault-tested against production-like data:

- current root-wide database versus per-session files under many concurrent sessions;
- one very large active session plus many cold sessions;
- WAL growth, checkpoint behavior, lock wait, page-cache/RSS, and bytes written;
- open/list/resume latency;
- large transcript, dense context edits, deep forks, and 100k terminal-task histories;
- output spooling/checkpoint cadence and process-loss boundaries;
- session backup/restore/archive/delete/clone behavior;
- interrupted session creation and catalog publication;
- catalog scan/repair cost and stale-path handling;
- corruption/failure isolation;
- schema archive/migration behavior;
- artifact publication, orphan cleanup, and retained receipt/tombstone rules.

The production migration should happen only after that evidence. P1 production work should keep the store boundary narrow enough that it does not further entrench the current one-database-per-root implementation.