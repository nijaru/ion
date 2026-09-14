# Source layout for the clean rewrite

Status: active implementation layout; K0-K5 implemented, 2026-09-13.

This document owns source/module organization for the clean rewrite described in `docs/core-runtime-migration.md`. It is subordinate to `DESIGN.md`: if the semantic architecture changes, this layout changes with it. File boundaries are not compatibility promises.

The goal is to prevent the new architecture from collapsing back into a few giant modules. The removed tree concentrated unrelated responsibilities into files such as `runtime/mod.rs`, `store/sql.rs`, `tui.rs`, and `main.rs`. The clean core deliberately keeps semantic ownership narrow.

## 1. Current workspace

Do not create a crate merely because a diagram can name one. Split crates only for a stable dependency/lifecycle boundary.

The active workspace is now:

```text
crates/
  ion-ai/         provider-neutral model contract + scripted service
  ion-core/       durable session/conversation/task kernel
  ion-terminal/   low-level terminal primitives; independently reviewed later
```

The old application crate is intentionally absent. Reintroduce `crates/ion/` only when a genuinely useful shell can be built on the new command/view/store boundaries. Do not restore it as a compatibility bridge to removed runtime APIs.

Potential later crates such as a protocol/server package or a separate execution-environment package require an accepted product boundary first. Do not pre-create them for symmetry.

## 2. `ion-core` current and target tree

Create modules only when real behavior belongs there. Current implementation has already landed the core K1-K3 files; later entries below remain targets.

```text
crates/ion-core/
  src/
    lib.rs
    id.rs
    artifact.rs

    conversation/
      mod.rs
      entry.rs
      input.rs
      context/
        mod.rs
        control.rs
        projection.rs
        fork.rs

    task/
      mod.rs
      invocation.rs
      record.rs
      output.rs
      kind.rs
      context.rs
      registry.rs
      typed.rs
      # phase.rs only if a typed authoring helper proves useful

    session/
      mod.rs
      owner.rs
      state.rs
      command.rs
      transaction.rs
      scheduler.rs
      idle.rs       # admission policy + queued-input scheduling
      lifecycle.rs  # graceful/fault close and abort cleanup admission
      wait.rs
      capacity.rs
      # add focused modules below when behavior is large enough:
      # handle.rs
      # cancellation.rs
      # recovery.rs

    view/
      mod.rs
      snapshot.rs   # SessionSnapshot + bounded SessionSummary/EntryPage
      event.rs
      # split further only as behavior grows

    store/
      mod.rs
      memory.rs
      # per-session SQLite store (implemented); artifact.rs is still future
      sqlite/
        mod.rs
        connection.rs
        schema.rs
        commit.rs
        conversation.rs
        entry.rs
        input.rs
        task.rs
        artifact.rs

    # K5 built-ins (implemented): generation, the one-call tool kind and the
    # post-tools join
    builtin/
      mod.rs
      generation.rs
      tool.rs
      post_tools.rs

  tests/
    k1_domain.rs
    k2_session.rs
    k3_driver.rs
    # later focused recovery/sqlite/worker suites
    support/
      # only when reusable deterministic fakes/fault injection are needed
```

This is not a request to create empty files. The current `session/owner.rs`, `session/transaction.rs`, and `session/scheduler.rs` are still within reviewable size, but should split by semantic owner as K4/K5 behavior lands rather than growing into new catch-all modules.

## 3. Module ownership

### `id.rs`

Only semantic identifiers and sequence/cursor types selected by R0.4.

No database helpers, runtime registries, or unrelated display policy.

### `conversation/`

Owns durable agent-thread semantics:

- `Conversation` record and history-parent/owner relationships;
- immutable transcript entry types;
- admitted input/result records;
- context controls and pure provider-neutral projection;
- fork visibility/cutoff logic.

It does **not** schedule tasks, execute models/tools, access SQLite directly, or own UI state.

`conversation/context/` is intentionally pure. Given durable entries plus a cutoff it derives effective provider-neutral context. It must not perform provider I/O or mutate canonical state.

### `task/`

Owns the generic durable work contract:

- task record/status/outcome/checkpoint shape;
- invocation kind/generation metadata;
- `TaskKind` async execute/recover/abort contract;
- invocation-scoped `TaskContext` and restricted `AbortContext`;
- output/scratch contract;
- task-kind registry;
- optional typed authoring adapter (`task/typed.rs`): `TypedHandler` erases into the same registry entry shape and the same scheduler/lifecycle. Decode failure is a terminal `Failed` only for never-dispatched work; for an already-dispatched task it interrupts and stays recoverable, and a result that cannot be encoded interrupts rather than terminalizing.

It does **not** own session scheduling or persistence. A task kind describes one recoverable operation; the session driver decides when an eligible task is invoked.

An optional phase helper may make complex typed checkpoints easier to author. It must compile to the ordinary task contract and cannot introduce another scheduler, lifecycle or persistence model.

### `session/`

Owns canonical session mutation and task driving.

Current split:

- `owner.rs`: resident session owner, persistence-before-install commit ordering, public mutation methods, snapshots and bounded observations;
- `state.rs`: resident records/indexes and semantic mutation application; persistence owns no copy of this state;
- `command.rs`: typed command/receipt/error vocabulary;
- `transaction.rs`: semantic mutation batches, read-your-writes draft state and invariant validation;
- `scheduler.rs`: task driver, registry dispatch, local invocation ownership, settlement dispatch and cancellation signaling;
- `lifecycle.rs`: graceful/fault close that fences writes before joining, and the separate bounded admission abort cleanup uses;
- `idle.rs`: the mode/state admission policy and the turn a conversation starts for queued input, including the `TurnTemplate` configuration. It owns when queued input becomes work, not how a turn is created (`owner.rs`/`transaction.rs`) or how it is driven (`scheduler.rs`);
- `wait.rs`: client task/dependency waits over committed-state wake signals;
- `capacity.rs`: independent process-local scarce-resource limits.

As behavior grows, split by real ownership:

- `handle.rs`: small client-facing command handle once a separate owner loop/channel exists;
- `cancellation.rs`: scope/barrier behavior when cancellation exceeds scheduler-local logic;
- `recovery.rs`: reopen classification and explicit resume/drive decisions;
- `lifecycle.rs`: open/close/fault/ownership transitions.

`session/mod.rs` remains wiring/re-exports. There is no `runtime.rs` catch-all module.

### `view/`

Owns bounded client projections, not canonical execution state:

- snapshots;
- a bounded `SessionSummary` and paginated fork-visible `EntryPage` reads, so summaries and scrolling do not materialize all history;
- committed events;
- observation/watch cursors and overflow/reset semantics.

K2 derives these reads in memory; K4 backs them with indexed storage queries.

TUI-specific focus, drafts, layout and rendering stay outside `ion-core`.

### `store/`

Owns persistence only.

The K4 prerequisite split is implemented. `Session` owns resident `SessionState`; `MemoryStore` is a volatile commit sink with no semantic records. A crate-private `Persistence` contract accepts validated batches. Transaction building prepares the next resident state; the writer installs it only after persistence succeeds, avoiding fallible semantic application after durability. Errors fence the owner and signal fault close to live drives.

Current dependency shape:

```text
Session owner
  resident SessionState
  transaction builder
  observations
  persistence store

commit
  build/validate against resident state
  -> durable store commit
  -> resident apply/index update
  -> observation publish
```

Keep the store interface crate-private and narrow. It is not a promise of interchangeable public database backends.

`store/sqlite/` owns all SQLite details. No other module imports `rusqlite`, raw SQL, or holds a SQLite connection. The per-session store is implemented; `Session::create`/`Session::open` are the public entry points and `SqliteStore` stays crate-private.

- `schema.rs`: schema/version/DDL only, currently version 4;
- `connection.rs`: open policy (WAL with `synchronous = FULL`, `busy_timeout`), create-versus-open, session identity and metadata reads;
- `commit.rs`: application of one atomic semantic mutation batch, including the commit-cursor compare-and-set that fences a stale writer authority;
- `conversation.rs`, `entry.rs`, `input.rs`, `task.rs`: focused per-record writes plus the reads open-time reconstruction composes;
- `artifact.rs`: publication/reference metadata integration, not arbitrary filesystem tools.

Reconstruction composes those per-record reads and never needs another record's payload. Entries keep `data`/`projection`/`context` as JSON columns. Tasks keep `input`/`checkpoint`/`invocation`/`outcome`/`output` as JSON columns and normalize identity, lifecycle state, dependencies and ownership into columns and child tables. Nothing stores a second copy of a value its columns already carry.

Do not create one giant `sql.rs` or `queries.rs` file.

### `builtin/`

Owns built-in task kinds, not kernel special cases.

Initial K5 built-ins:

- generation;
- tool execution wrapper;
- post-tools/join continuation.

K6 adds `worker.rs`: the trusted adapter that spawns a retained worker conversation. Like the others it is an ordinary registered kind, and it reaches no session state the ordinary plan path does not.

The generic scheduler must not branch on these names. If a built-in needs specialized behavior, it uses the same task capabilities available to an appropriate registered kind.

Implemented as `builtin::{generation, tool, post_tools}`. `Builtins` registers all three under canonical names over one `ion_ai::ModelService` and one `ToolCatalog`; a client composes them like any other kind. `tool.rs` also owns the `Tool`/`ToolCatalog` seam that tool execution receives through.

Worker creation is primarily conversation ownership/admission mediated by a trusted task/tool capability, not a separate swarm runtime.

### `artifact.rs`

Owns core artifact identity/reference/integrity semantics. Large-output file mechanics may later move behind a more specific store/environment boundary if evidence warrants it.

## 4. Dependency direction

Keep dependencies one-way enough that a module can be understood without the entire application.

```text
ion-ai contract
        |
        v
conversation records/context       task contract
        |                              |
        +--------------+---------------+
                       v
                    session
                 owner/scheduler
                       |
                       v
                     store

built-in task kinds ---> task contract + conversation/context + ion-ai
view projections ------> committed core records
```

More precisely:

- model/provider code never imports `SessionId`, `TaskId`, store commands or SQLite types;
- `conversation/context` stays pure and side-effect free;
- `task` contracts do not import the concrete session owner;
- `store` never calls models, tools, processes or UI;
- built-in task kinds never execute SQL directly;
- session scheduling never parses provider wire formats;
- terminal/client code mutates canonical state only through session commands/handles;
- provider/environment callbacks cannot directly publish canonical session events.

When a proposed dependency violates these directions, treat that as a design smell before adding an abstraction to hide it.

## 5. `ion-ai`

`ion-ai` is an implemented small independent crate, not a future placeholder. Its current job is the provider-neutral contract and scripted deterministic service. It contains no production HTTP provider catalogue yet.

Later provider/auth work may add logical areas such as:

```text
auth/
catalog/
provider/
api/
```

Provider and API are separate concepts:

- provider = identity, endpoint policy, auth, model catalogue/availability;
- API adapter = wire request/stream protocol and error decoding.

Several providers may reuse one API adapter. The model service exposes typed normalized failures; durable retry policy remains in generation-task logic.

If an API adapter grows large, split by request/stream/types/error rather than making one provider file responsible for all four.

## 6. Future application/TUI layout

Do not port the removed giant TUI application in place. Rebuild the application as a client of the command/view API after the core boundary stabilizes.

Likely shape:

```text
crates/ion/src/
  main.rs              # argument parsing + composition only
  cli.rs
  config/
  app/
    mod.rs
    session.rs
  tui/
    mod.rs
    app.rs
    state.rs
    reducer.rs
    input.rs
    render/
    overlay/
  acp/                 # only after protocol pass
  export/              # only after format pass
```

`main.rs` must not become an application state machine. It constructs services, dispatches the requested frontend and handles process-level exit/error reporting.

`ion-terminal` remains a low-level terminal/editor/renderer dependency. It must not know about conversations, tasks or model providers.

## 7. File-size and cohesion rules

Size is a warning signal, not an architecture metric by itself. Still, the rewrite should make pathological growth visible early.

For hand-written Rust source:

- target most files at roughly **100-500 lines**;
- at **~700-800 lines**, review whether the file contains multiple ownership/change reasons;
- above **~1,000 lines**, splitting is the default unless the file is generated data, a schema/table definition, or a deliberately cohesive parser/state machine with a recorded reason;
- `lib.rs`/`mod.rs` should mostly declare modules, re-export public types and contain concise module-level documentation;
- do not solve size limits by creating `common.rs`, `utils.rs`, `types.rs`, or `helpers.rs` dumping grounds.

Tests count toward maintainability too. Prefer scenario-focused integration-test files instead of one omnibus regression file.

The legacy large files are gone. Add a lightweight CI/source-size report only when it becomes useful; do not turn line-count thresholds into an architecture substitute.

## 8. Naming rules

Prefer names that reveal semantic ownership:

```text
session/owner.rs
session/scheduler.rs
conversation/context/projection.rs
store/sqlite/commit.rs
task/registry.rs
```

Avoid generic buckets:

```text
runtime.rs
manager.rs
engine.rs
common.rs
utils.rs
helpers.rs
state.rs      # unless the module really owns one explicit state object
```

`manager` is especially discouraged when the object actually has a precise role such as registry, owner, store, scheduler or directory.

## 9. Test organization

Port historical tests by invariant, not by old module name.

Examples:

- lane retry tests become input-admission tests;
- operation cancellation tests become task/session cancellation tests;
- effect crash tests become task-recovery tests;
- family/subagent tests become owned-conversation/worker tests.

Use `tests/support/` for deterministic fakes and fault injection only when several focused suites need them. Test support must not become another runtime implementation.

Keep process-death tests as actual subprocess termination once K4 persistence exists. Keep storage barriers, controlled clocks and scripted model/environment services reusable across focused tests.

## 10. Reference posture

Pico is evidence for keeping the durable harness foundation small and separating provider/model machinery from agent state. Its exact TypeScript modules and generic scoped-state framework are not Ion APIs.

Codex is evidence for production concerns and Rust implementation patterns, but not for source-file size or module topology by default. Ion should deliberately avoid accumulating large multi-responsibility files even when a mature upstream does so.

The governing rule is simple:

> A module should have one semantic owner and a small number of reasons to change. When a new feature introduces a new owner, create a boundary instead of extending the nearest large file.
