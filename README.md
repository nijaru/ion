# Ion

Ion is building a provider-neutral Rust coding agent with a first-class terminal
interface. It runs one primary conversation by default, with optional cooperating
worker conversations. Pi/Pico and Codex are engineering references, not
compatibility targets.

## Current status

The workspace builds three libraries:

- `ion-core`: the durable turn engine. A session owns conversations, immutable
  entries, accepted inputs and the turns that answer them, backed by per-session
  SQLite storage on a dedicated database thread.
- `ion-ai`: provider-neutral model contracts and a scripted model service.
- `ion-terminal`: low-level terminal components.

The current engine implements the first durable-turn slice: request-key replay,
frozen per-step request bases, response-ready crash recovery, sequential scripted tool
execution, conservative unknown outcomes, supervised stop/join behavior, exclusive
session ownership and bounded pages/content. A scripted model/tool exchange runs end to
end through the headless API.

The accepted [architecture](ARCHITECTURE.md) was deliberately refined on 2026-09-18
before real providers and native tools made the early v0 boundaries expensive to change.
The current Rust is therefore **pre-cutover**, not the final internal API. The next
cutover keeps the coding Turn but adds stable per-turn provider/tool/execution bindings,
versioned semantic request manifests, explicit effect admission and recoverable backend
start receipts, logical ToolInvocations with immutable physical ToolAttempts, durable
outcome staging for safely parallel tool batches, external execution truth separate from
the model-visible result, typed drive/session health and atomic commit-addressed update
batches. The longer-horizon target anchors context in immutable ContextBoundary
entries (exact retained Input references + typed checkpoint + lossless recent tail),
uses a host-owned cross-process workspace registry, and keeps fresh-vs-forked worker
context separate from joined-vs-retained lifetime. Source-confirmed races are acceptance tests for
that cleaner shape rather than reasons to preserve the existing one.

The current source also has an opt-in workspace wrapper,
`Workspace::open(root)?.bind(tool)`, backed by `.ion/claims.sqlite`. It conservatively
retains a mutation claim across uncertainty, panic and process loss. The revised target
keeps that safety property but moves claims/reconciliation behind the structured host
execution boundary so a ToolAttempt records the execution receipt explicitly instead of
hiding it inside a Tool wrapper. There is no automatic expiry or force-clear policy.

This is **unconfined coordination**, not approval or sandbox enforcement. Hosts
must use the same canonical root and bind tools to that actual environment.
Opening a root nested below an already-coordinated workspace is refused, because
two coordinators over one tree would each serialize only their own writers; a
coordinator created after an outer workspace was opened is still not discovered,
so keep one root per tree. External writers and tools that delete the
coordinator can bypass it. Preserve the coordinator files across restarts.
Approval/revocation and evidence-based reconciliation are not implemented.

Closing interrupts active work without itself cancelling the unfinished turn.
After reopening, explicit resume continues the turn; uncertain tool outcomes still
require resolution rather than automatic repetition.

Still missing: real provider adapters, a runnable `ion` binary, workspace tools
(read/edit/exec), context compaction and forking, the terminal UI, and workers.
No live-provider effectiveness has been measured.

**There is no runnable `ion` binary in the current workspace.** The legacy
`crates/ion/` application source is reference material outside the workspace; its
CLI, provider configuration and usage instructions do not describe the new core.
`cargo run -p ion` is not supported at this revision.

## Development

The checked-in toolchain pins Rust 1.98.0 and the required components.

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

The turn-engine regressions are grouped by boundary:

```sh
cargo test --locked -p ion-core --test c1_turn          # admission, steps, tools, queues
cargo test --locked -p ion-core --test c1_cancellation  # cancellation precedence, uncertainty
cargo test --locked -p ion-core --test c1_storage       # ownership, schema, corruption, pages
cargo test --locked -p ion-core --test c2_execution     # stop, join, close, late evidence
cargo test --locked -p ion-core --test c2_workspace     # claims, cross-session conflict, process loss
cargo test --locked -p ion-core --lib                   # commit faults, recovery boundaries
```

These exercise the libraries against scripted services; they are not evidence of
live-provider effectiveness or a usable terminal application.

## Project documentation

- [ARCHITECTURE.md](ARCHITECTURE.md): target contracts, ownership and failure semantics.
- [AGENTS.md](AGENTS.md): repository working instructions.

Earlier architectures and implementation history remain in Git. They are not
compatibility targets. Research notes and development planning are not public
architecture contracts.

## License

[MIT](LICENSE)
