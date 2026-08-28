# Architecture normalization plan

Status: active on `refactor/architecture-normalization`

This pass fixes architectural boundaries before the general Rust/code-hygiene
pass. It is not a feature-parity project and it does not replace Ion's durable
single-writer execution model.

## Invariants that stay

- One mutation authority per loaded session.
- `OperationMachine` remains pure: no Tokio, filesystem, network, provider, or
  tool I/O.
- Accepted user intent is durable before acknowledgment.
- Repeat-sensitive external effects commit exact intent before execution and
  settle afterward.
- Slow provider/tool/subprocess I/O runs outside the session mutation line.
- Local semantic history remains canonical; model context is a projection.
- Frontends consume session handles/snapshots/live events rather than owning a
  second transcript or runtime.

## Phase 1 — normalize vocabulary and module boundaries

- [x] Make `runtime`, `store`, and `tool` normal directory modules.
- [x] Establish canonical model vocabulary:
  `ResolvedModel`, `ModelRequest`, `ModelEvent`.
- [x] Establish migration names for event/session-instance/budget/child refs.
- [ ] Remove compatibility aliases after repository callers use the canonical
  names; aliases are migration scaffolding, not permanent API surface.
- [ ] Rename context vocabulary once the model-step boundary is typed:
  prefer `PromptProjection`/`ModelMessage` over generic `Context*` where the
  value is specifically model-facing.

## Phase 2 — make the crash boundary strongly typed

- [x] Fix recovery so an existing tool effect never invents a new operation id.
- [x] Add one typed durable-effect codec (`DurableEffect`).
- [x] Make recovery decode through the typed boundary and replay with one
  `next_attempt()` path.
- [ ] Make all effect creation use typed constructors; normal runtime code must
  not assemble `kind` strings or `serde_json::json!` effect payloads.
- [ ] Make SQLite validation consume typed effect values rather than inspect
  arbitrary JSON fields.
- [ ] Make the raw `kind` + JSON storage representation private to the storage
  codec. The SQLite schema may remain inspectable without making its encoding
  an application-level contract.

## Phase 3 — separate tool description, preparation, policy, and execution

Target flow:

```text
model ToolCall
  -> capability snapshot lookup
  -> Tool::prepare / ToolPreparer
  -> PreparedToolCall
  -> policy decision over prepared action
  -> durable effect intent
  -> execute exactly the prepared invocation
  -> durable settlement
```

A `PreparedToolCall` owns the information the runtime currently reconstructs by
matching tool names:

- stable capability identity/generation;
- validated/canonical execution input;
- policy-facing action/target;
- recovery policy and typed reconciliation evidence when applicable;
- model-facing call identity.

Completion criteria:

- [ ] `SessionRuntime` contains no checks for `"write"`, `"edit"`, `"bash"`,
  `"delegate"`, `"spawn_child"`, etc.
- [ ] `ToolRegistry::canonicalize` does not infer semantic classes from tool
  names.
- [ ] Remove `__ion_reconciliation` and `__ion_artifact_root` magic arguments.
- [ ] Native, MCP, extension, and child-control tools use the same prepared-call
  admission contract.
- [ ] Structural capabilities are represented by metadata/policy class rather
  than hard-coded names.

## Phase 4 — simplify operation state

`OperationMachine` should describe durable conversational execution semantics,
not harness heuristics.

- [ ] Replace compaction-specific parallel state branches with a typed model
  invocation purpose where possible (`Assistant`, `Compaction`, future bounded
  maintenance purposes).
- [ ] Move decisions such as when to compact and reserve thresholds into a
  harness/projection policy outside the reducer.
- [ ] Keep the fact that a compaction invocation is pending/settled durable.
- [ ] Revisit prompt/root-input duplication: an already-applied root message
  should not also masquerade as pending inbox work.
- [ ] Bind capability snapshots to the model invocation that exposed them,
  rather than treating them as generic operation state.

## Phase 5 — partition the session owner without creating more owners

Keep one Tokio task and one writer. Split its state by semantic lifetime:

```text
SessionRuntime
  DurableSessionState   canonical entries, operation queue, selection
  DriverState           active invocation/effect/budget/maintenance
  LiveState             drafts, live tools, latest presentation data
  SessionServices       store, model service, capabilities, policy
```

- [ ] Move these groups into explicit structs.
- [ ] Split `runtime/mod.rs` by responsibility (`session`, `drive`, `command`,
  `live`, `effects`, `recovery`, `persistence`) while keeping one owner.
- [ ] Avoid `Arc<Mutex<SessionState>>`, per-module actors, or task-per-module
  designs.

## Phase 6 — make `Runtime` truthful

The process runtime should actually own the loaded-session registry and shared
services. A per-session incarnation should not be named `Runtime` merely
because it has a task handle.

Target:

```text
Runtime
  shared model/provider service
  SessionStore
  capability/extension/MCP services
  SessionRegistry<SessionId, SessionTask>

SessionTask
  SessionRuntime single writer
  SessionHandle
```

- [ ] Add real create/open/get/close session registry semantics.
- [ ] Remove the current one-session `RuntimeHandle`/`SessionHandle` overlap.
- [ ] Route child session creation/open/resume through the same registry.
- [ ] Keep child lineage/budget/capability narrowing explicit.

## Phase 7 — separate generic child sessions from delegation tools

- [ ] Generic child-session service owns create/open/observe/cancel/resume/fork.
- [ ] Model-facing delegation tools adapt that service; they do not own runtime
  topology.
- [ ] Split the current large `delegate.rs` accordingly.

## Phase 8 — harness experimentation layer

Only after the correctness/ownership boundaries above are stable:

- [ ] Introduce a versioned `HarnessProfile` frozen with each model invocation.
- [ ] Put prompt/projection/tool-presentation/compaction/retrieval heuristics in
  profile-owned strategy objects, not `SessionRuntime` conditionals.
- [ ] Add `ion-eval` for cost-matched, held-out comparisons and recovery/crash
  conformance tests.

## Code-hygiene pass follows architecture

After this plan is complete or the remaining boundaries are independently
stable, audit:

- unnecessary clones/allocations;
- sync mutex use and lock scopes;
- `expect`/panic policy and impossible-state handling;
- error enums and stringly errors;
- API visibility/re-export surface;
- async cancellation and task drainage;
- test duplication and brittle timing;
- comments that restate implementation rather than invariants;
- stale compatibility aliases/dead abstractions/generated-code residue.

Do not mix broad cosmetic cleanup into architectural commits unless it is
required to keep formatting, Clippy, or tests green.
