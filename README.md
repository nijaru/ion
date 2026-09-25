# Ion

Ion is building a provider-neutral Rust coding agent with a first-class terminal
interface. It runs one primary conversation by default, with optional cooperating
worker conversations. Pi/Pico and Codex are engineering references, not
compatibility targets.

## Current status

The workspace builds three libraries:

- `ion-core`: the replacement durable Turn runtime and storage layer.
- `ion-ai`: provider-neutral model contracts and scripted provider fixtures.
- `ion-terminal`: low-level terminal components.

The maintained `ion-core` no longer contains the prototype Session/task/tool/workspace
runtime. The replacement branch implements the R1 durable domain, Session/provider
foundation, and an initial tool-execution boundary:

- fresh SQLite schema v5 with one Session-local identity sequence and exact commit cursor;
- revisioned conversation configuration, conversation-scoped idempotent input admission,
  inline immutable `TurnEnvironment`, constrained `TurnSettings`, and one unfinished Turn
  per conversation;
- a dedicated bounded SQLite command thread with WAL, `synchronous=FULL`, foreign keys,
  strict durable decoding, and mutation fencing after ambiguous persistence;
- semantically passive `Session::open()`: reopening performs no provider/tool
  reconciliation, dispatch, retry, timer work, worker start, workspace claim, or recovery write;
- exact `CommitReceipt { seq, update }` publication and bounded subscribe-before-snapshot
  observation with overflow/resnapshot semantics and paginated older history;
- explicit supervised `resume()`, typed `DriveExit`, process-local `SessionHealth`,
  per-Turn effect gates retired after terminal settlement, and cancellation generation
  linearization;
- logical `ModelStep` versus physical `ModelAttempt`, durable response-ready evidence,
  monotonic start-receipt/evidence refinement, selection guarded by current Turn generation and
  step eligibility, and atomic predecessor-superseding provider fallback;
- stable provider effect keys derived from Session + Turn + step ordinal and covered by the
  provider-request fingerprint when adapters use them as idempotency material; exact frozen
  service-realm matching and host-owned credential/egress preflight before intent, with a
  second live check before adapter start;
- frozen tool-schema validation and persisted `PreparedAction` admission; digest-bound
  action authority checked against the frozen ceiling before execution intent; distinct physical
  tool attempts, conservative receipt recovery, nonoverlapping retries, durable outcome staging,
  source-order results, and model-context/storage closure reserves before execution;
- explicit unknown-result acceptance without rewriting execution evidence, late reconciliation,
  and cancellation that closes the tool exchange without claiming uncertain work stopped;
- a host-owned cross-process `workspace_registry` outside the checkout, with frozen Unix
  filesystem/repository identity, durable mutation claims, revision checks, and orphan
  quarantine that survives Session/blob deletion. Trusted hosts authenticate resolution
  evidence; this coordinates cooperating writers, not confined execution.

Opening and inspection are passive; explicit resume is the boundary that may reconcile a
persisted provider attempt and start new provider work. Closing seals local effect admission,
signals and joins locally owned drive work, then releases storage ownership without silently
turning suspended work into user cancellation.

`resume_with_tools` accepts exact compatible host tool implementations; missing bindings
park before a new provider dispatch. After a completed provider response is durable, a
missing exact preparer instead records an unavailable invocation and a source-order error
result without fabricating an action or physical attempt; subsequent requests still park
until the selected loadout resolves. The host tool boundary checks live authority before
execution intent; denial parks without spending a physical attempt. It must recheck at
actual effect admission because permission can change between those points. `tool_records`
inspects attempts, `accept_tool_unknown` settles an uncertain exchange, and
`reconcile_tools` recovers evidence without dispatching. Live host policy returns
`Allow`, `Ask` or `Deny`: `Ask` parks on a durable per-invocation approval bound to its
exact action, executor, workspace and expiry. An authenticated host can call
`decide_tool_approval`; explicit denial stages a truthful result without an attempt.
Duplicate decisions do not advance the commit cursor. Active tool records are included
in bounded snapshot/watch hydration.

Tool execution is sequential. Scripted tests cover an actual owner-process kill after a
filesystem mutation and host receipt, followed by passive reopen and explicit reconciliation
without reexecution. These fixtures are not native tools or a confinement implementation.
A host backend must enforce current authority, workspace claims, and stop/join behavior.
The deleted `.ion/claims.sqlite` wrapper is not part of the replacement runtime.

There is no compatibility bridge or hybrid old/new runtime. Earlier unreleased schemas
(v1–v4) are refused rather than migrated; Git retains the prototype and its useful failure
scenarios are being restored against the replacement owners.

Still missing: native read/edit/exec backends, a user-facing approval client and
host authentication/policy backend, Session-integrated artifact publication and GC,
parallel tool dispatch, context compaction/forking, Steer/InteractionReply placement,
real provider adapters, a runnable `ion` binary, the terminal UI, and workers.
`submit_turn` atomically admits text and places its Turn with one watch receipt;
request-key replay is idempotent and an insertion fault rolls back the submission.
Unimplemented Steer/InteractionReply inputs reject at admission rather than queue
unconsumable work. Tool results distinguish complete inline, complete artifact and
incomplete capture. An oversized backend value becomes an explicit incomplete output
warning, preserving its terminal effect evidence without permitting replay. Complete
artifacts remain unavailable until publication is wired. An immutable bounded
BlobStore foundation is not wired to Session settlement, publication evidence,
artifact paging, or GC. It cannot yet back large
native output. Its host-owned namespace must be outside agent-writable workspace state and
protected from untrusted same-user processes; a BlobStore owner lock excludes a second
handle, while the host still owns namespace ancestry and filesystem trust.
Provider admission is a local host callback, not network confinement or a production
credential policy; real adapters must enforce realm/credential validity at actual I/O.
A configured monetary cap parks before physical attempt intent until a host can supply
a conservative cost quote. Request and terminal provider-response capacity checks stop
encoding at their frozen limits rather than allocating complete oversized JSON copies. No live-provider effectiveness has been
measured.

**There is no runnable `ion` binary in the current workspace.** The legacy
`crates/ion/` application source is reference material outside the workspace; its CLI,
provider configuration and usage instructions do not describe the new core.
`cargo run -p ion` is not supported at this revision.

## Development

The checked-in toolchain pins Rust 1.98.0 and the required components.

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

The replacement runtime regressions currently live here:

```sh
cargo test --locked -p ion-core --test r1b_storage  # admission/config/Turn/store/watch
cargo test --locked -p ion-core --test r1b_drive    # provider drive/cancellation/recovery/fallback
cargo test --locked -p ion-core --test c1_tools     # tool exchanges/receipts/closure/process loss
cargo test --locked -p ion-core --test r1c_workspace_registry # claims/identity/process loss
cargo test --locked -p ion-core --lib               # domain/schema/request/observation contracts
```

These are deterministic library/fixture checks; they are not evidence of live-provider
effectiveness or a usable terminal application.

## Project documentation

- [ARCHITECTURE.md](ARCHITECTURE.md): target contracts, ownership and failure semantics.
- [AGENTS.md](AGENTS.md): repository working instructions.

Earlier architectures and implementation history remain in Git. They are not
compatibility targets. Research notes and development planning are not public
architecture contracts.

## License

[MIT](LICENSE)
