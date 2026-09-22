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
runtime. The replacement branch now implements the R1 durable domain and schema plus the
R1B Session/provider foundation:

- fresh SQLite schema v2 with one Session-local identity sequence and exact commit cursor;
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
  per-Turn effect gates, and cancellation generation linearization;
- logical `ModelStep` versus physical `ModelAttempt`, durable response-ready evidence,
  monotonic start-receipt/evidence refinement, selection guarded by current Turn generation and
  step eligibility, and atomic predecessor-superseding provider fallback;
- stable provider effect keys derived from Session + Turn + step ordinal and covered by the
  provider-request fingerprint when adapters use them as idempotency material;
- a host-owned cross-process `workspace_registry` outside the checkout, with frozen Unix
  filesystem/repository identity, durable mutation claims, revision checks, and orphan
  quarantine that survives Session/blob deletion. Trusted hosts authenticate resolution
  evidence; this coordinates cooperating writers, not confined execution.

Opening and inspection are passive; explicit resume is the boundary that may reconcile a
persisted provider attempt and start new provider work. Closing seals local effect admission,
signals and joins locally owned drive work, then releases storage ownership without silently
turning suspended work into user cancellation.

Native tool execution is deliberately not connected yet. The replacement
`ToolBinding`/`ToolInvocation`/`ToolAttempt` domain and schema exist, but an active tool
loadout currently parks before provider dispatch. R1C will add deterministic PreparedAction
admission, immutable physical tool attempts, source-order result materialization, execution
receipts, and integration with the host-owned WorkspaceRegistry. The registry is available
independently; native tool execution does not use it yet. The deleted `.ion/claims.sqlite`
workspace wrapper is not part of the replacement runtime.

There is no compatibility bridge or hybrid old/new runtime. Schema v1 is refused rather than
migrated; Git retains the prototype and its useful failure scenarios are being restored against
the replacement owners.

Still missing: native read/edit/exec tools, tool/registry integration, BlobStore,
context compaction/forking, real provider adapters, a runnable `ion` binary, the terminal UI,
and workers. No live-provider effectiveness has been measured.

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
