# Source layout for the clean rewrite

Status: active implementation layout, 2026-09-12.

This document owns source/module organization for the clean rewrite described in `docs/core-runtime-migration.md`. It is subordinate to `DESIGN.md`: if the semantic architecture changes, this layout changes with it. File boundaries are not compatibility promises.

The goal is to prevent the new architecture from collapsing back into a few giant modules. The old tree concentrated unrelated responsibilities into files such as `runtime/mod.rs`, `store/sql.rs`, `tui.rs`, and `main.rs`. Their GitHub-reported sizes are bytes, not line counts, but the concentration is still evidence that ownership boundaries were too broad.

Current Pico is useful evidence in the opposite direction: its clean-room foundation is split into narrow modules for entries, tasks, runtime capabilities, addresses and the core, rather than one monolithic harness file. Codex remains useful production evidence but also demonstrates that mature Rust code can still accumulate very large files if boundaries are not actively maintained. Ion should keep the small-kernel discipline deliberately.

## 1. Workspace rule

Do not create a crate merely because a diagram can name one. Split crates only for a stable dependency/lifecycle boundary.

Leading staged workspace:

```text
crates/
  ion-core/       durable agent/session kernel
  ion-terminal/   low-level terminal primitives; independently reviewed later
  ion/            application shell; old implementation is removed/rebuilt after core cutover

  # introduced by R0.5 if the boundary holds
  ion-ai/         provider-neutral model API first; providers/auth/catalog later
```

Potential later crates such as a protocol/server package or a separate execution-environment package require an accepted product boundary first. Do not pre-create them for symmetry.

### Clean-rewrite cutover

After R0.1-R0.5 settle:

- replace `ion-core` directly;
- do not keep legacy and target runtime modules side by side;
- remove old `ion` application code that only exists to drive the deleted runtime, or temporarily remove the app crate from the workspace until a minimal new shell is useful;
- keep `ion-terminal` only insofar as it remains independently valid and compiling;
- keep old behavior in Git history and in ported regression scenarios, not in compatibility adapters.

A green workspace is still required. It is acceptable for the user-facing binary to be temporarily absent or minimal while the core is reconstructed.

## 2. `ion-core` target tree

Start with this shape after R0, adding modules only when the behavior exists:

```text
crates/ion-core/
  src/
    lib.rs
    id.rs

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
      kind.rs
      context.rs
      output.rs
      phase.rs          # optional authoring helper, not another scheduler
      registry.rs

    session/
      mod.rs
      handle.rs
      owner.rs
      command.rs
      transaction.rs
      scheduler.rs
      cancellation.rs
      recovery.rs
      lifecycle.rs

    view/
      mod.rs
      snapshot.rs
      event.rs
      watch.rs

    store/
      mod.rs
      memory.rs         # deterministic tests/prototypes; not a second product backend
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

    builtin/
      mod.rs
      generation.rs
      tool.rs
      post_tools.rs
      # collapse.rs / job.rs only when implemented

    artifact.rs

  tests/
    admission.rs
    task_recovery.rs
    cancellation.rs
    context_forks.rs
    workers.rs
    sqlite_recovery.rs
    support/
      mod.rs
      model.rs
      store.rs
      clock.rs
      crash.rs
```

This is a target organization, not a request to create empty files. Create a module when the first real behavior belongs there.

## 3. Module ownership

### `id.rs`

Only semantic identifiers and sequence/cursor types selected by R0.4.

No database helpers, display policy beyond stable formatting, or runtime registries.

### `conversation/`

Owns durable agent-thread semantics:

- `Conversation` record and history-parent/owner relationships;
- immutable transcript entry types;
- admitted input/result records;
- context controls and pure provider-neutral projection;
- fork visibility/cutoff logic.

It does **not** schedule tasks, execute models/tools, access SQLite directly, or own UI state.

`conversation/context/` is intentionally pure. Given durable entries plus a cutoff it derives the effective provider-neutral context. It must not perform provider I/O or mutate canonical state.

### `task/`

Owns the generic durable work contract:

- task record/status/outcome/checkpoint shape;
- `TaskKind` trait;
- invocation-scoped task capabilities exposed by `TaskContext`;
- output/scratch contract;
- optional typed phase helper;
- task-kind registry contract.

It does **not** contain the session scheduler. A task kind describes one recoverable operation; the session owner decides when an eligible task is invoked.

The optional `phase` helper must compile to the ordinary task contract. It cannot introduce another lifecycle, scheduler or persistence model.

### `session/`

Owns canonical session mutation and scheduling.

- `handle.rs`: small public/client-facing session handle and command methods;
- `owner.rs`: the one serialized semantic mutation owner and its run loop;
- `command.rs`: typed command/receipt vocabulary;
- `transaction.rs`: bounded semantic mutation builder and invariant validation;
- `scheduler.rs`: readiness, dependencies, invocation reservation and capacity;
- `cancellation.rs`: durable cancellation marks/fencing and scope rules;
- `recovery.rs`: reopen classification and explicit resume/drive decisions;
- `lifecycle.rs`: open/close/fault/ownership transitions.

`session/mod.rs` is wiring/re-exports, not an implementation dump.

There is no `runtime.rs` catch-all module.

### `view/`

Owns bounded client projections, not canonical execution state:

- snapshots;
- committed events;
- watch/subscription cursors and overflow/reset semantics.

TUI-specific focus, drafts, layout and rendering stay outside `ion-core`.

### `store/`

Owns persistence only.

`store/mod.rs` exposes one crate-private semantic store interface used by the session owner. It is deliberately narrower than a generic database framework.

`memory.rs` exists only to make deterministic kernel tests cheap. It is not a promise of interchangeable production storage backends.

`store/sqlite/` owns all SQLite details. No other module imports `rusqlite`/raw SQL or holds a SQLite connection.

- `schema.rs`: schema/version/DDL only;
- `connection.rs`: connection pragmas/open/close/transaction setup;
- `commit.rs`: application of one atomic semantic mutation batch;
- per-record modules: focused point/range reads and persistence helpers for that record family;
- `artifact.rs`: publication/reference metadata integration, not arbitrary filesystem tools.

Do not create one giant `sql.rs` or `queries.rs` file.

### `builtin/`

Owns built-in task kinds, not kernel special cases.

Initial built-ins:

- generation;
- tool execution wrapper;
- post-tools/join continuation.

The generic scheduler must not branch on these names. If a built-in needs specialized behavior, it uses the same task capabilities available to an appropriate registered kind.

Worker creation is primarily conversation ownership/admission mediated by a tool/task capability, not a separate swarm runtime.

### `artifact.rs`

Owns core artifact identity/reference/integrity semantics. Large-output file mechanics may later move behind a more specific store/environment boundary if evidence warrants it.

## 4. Dependency direction

Keep dependencies one-way enough that a module can be understood without the entire application.

```text
ion-ai contract (if R0.5 accepts it)
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

- model/provider code must never import `SessionId`, `TaskId`, store commands or SQLite types;
- `conversation/context` must be pure and side-effect free;
- `task` contracts must not import the concrete session owner;
- `store` must not call models, tools, processes or UI;
- `builtin` task kinds must not execute SQL directly;
- session scheduling must not parse provider wire formats;
- terminal/client code must not mutate canonical state except through session commands;
- provider/environment callbacks cannot directly publish canonical session events.

When a proposed dependency violates these directions, treat that as a design smell before adding an abstraction to hide it.

## 5. `ion-ai` leading layout after R0.5

If R0.5 confirms a clean model boundary, make it a small independent crate immediately rather than rebuilding provider knowledge inside `ion-core` or the binary crate.

Initial contract-only tree:

```text
crates/ion-ai/src/
  lib.rs
  model.rs
  message.rs
  content.rs
  tool.rs
  request.rs
  response.rs
  usage.rs
  error.rs
  service.rs
  scripted.rs
```

The first version contains no production HTTP provider catalog. `scripted.rs` exists for deterministic agent tests.

Later provider/auth pass may add:

```text
  auth/
    mod.rs
    credential.rs
    store.rs
    oauth.rs

  catalog/
    mod.rs
    model.rs
    cache.rs

  provider/
    mod.rs
    registry.rs
    openai.rs
    anthropic.rs
    openrouter.rs
    local.rs
    ...

  api/
    openai_responses/
    openai_compatible/
    anthropic_messages/
    google/
    ...
```

Provider and API are separate concepts:

- provider = identity, endpoint policy, auth, model catalogue/availability;
- API adapter = wire request/stream protocol and error decoding.

Several providers may reuse one API adapter. The model service exposes typed normalized failures; durable retry policy remains in generation-task logic.

If an API adapter grows large, split by `request`, `stream`, `types` and `error` rather than making one provider file responsible for all four.

## 6. Application/TUI layout after the core boundary stabilizes

Do not port the current giant `tui.rs` in place. Rebuild the application as a client of the command/view API.

Likely application organization:

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
      mod.rs
      transcript.rs
      group.rs
      composer.rs
      status.rs
      approvals.rs
    overlay/
      mod.rs
      model.rs
      settings.rs
      help.rs
  acp/                 # only after protocol pass
  export/              # only after format pass
```

`main.rs` should never become an application state machine. It constructs services, dispatches the requested frontend and handles process-level exit/error reporting.

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

Once the fresh core replaces the old one, add a lightweight CI/source-size check that reports files crossing the review threshold. Do not make the current legacy files satisfy the new threshold before they are deleted.

## 8. Naming rules

Prefer names that reveal semantic ownership:

Good:

```text
session/owner.rs
session/cancellation.rs
conversation/context/projection.rs
store/sqlite/commit.rs
task/phase.rs
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

Port old tests by invariant, not by old module name.

Examples:

- lane retry tests become input-admission tests;
- operation cancellation tests become task/session cancellation tests;
- effect crash tests become task-recovery tests;
- family/subagent tests become owned-conversation/worker tests.

Use `tests/support/` for deterministic fakes and fault injection only. Test support must not become another runtime implementation.

Keep process-death tests as actual subprocess termination. Keep storage barriers, controlled clocks and scripted model/environment services reusable across focused tests.

## 10. Reference posture

Pico is evidence for keeping the durable harness foundation small and separating provider/model machinery from agent state. Its exact TypeScript modules and generic scoped-state framework are not Ion APIs.

Codex is evidence for production concerns and Rust implementation patterns, but not for source-file size or module topology by default. Ion should deliberately avoid accumulating large multi-responsibility files even when a mature upstream does so.

The governing rule is simple:

> A module should have one semantic owner and a small number of reasons to change. When a new feature introduces a new owner, create a boundary instead of extending the nearest large file.
