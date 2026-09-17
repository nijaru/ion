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

The engine implements the accepted [turn contracts](ARCHITECTURE.md): durable
admission with request-key replay, frozen request bases, response-ready evidence
that survives a crash without a second provider call, sequential tool execution,
cancellation whose only winner is a committed turn success, truthful results for
uncertain actions, supervisor-owned tool execution with stop, join and
retries-after-close semantics, exclusive session ownership, and bounded pages and
content budgets. A scripted model/tool exchange runs end to end through the
headless API.

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
