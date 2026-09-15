# Ion

Ion is building a provider-neutral Rust coding agent with a first-class terminal
interface. It runs one primary conversation by default, with optional cooperating
worker conversations. Pi/Pico and Codex are engineering references, not
compatibility targets.

## Current status

The workspace currently builds three libraries:

- `ion-core`: durable sessions, conversations, immutable entries, inputs and
  recoverable tasks, backed by per-session SQLite storage.
- `ion-ai`: provider-neutral model contracts and a scripted model service.
- `ion-terminal`: low-level terminal components.

The core supports a scripted generation/tool chain, cancellation and recovery,
request cutoffs, and durable conversation configuration. A real-provider coding
loop, bounded storage residency, and the rebuilt application/TUI remain unfinished.
See [ROADMAP.md](ROADMAP.md) for implemented behavior and acceptance evidence.

**There is no runnable `ion` binary in the current workspace.** The legacy
`crates/ion/` application source remains as reference material outside the
workspace; its CLI, provider configuration and usage instructions do not describe
the new core. `cargo run -p ion` is not supported at this revision.

## Development

The checked-in toolchain pins Rust 1.98.0 and the required components.

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

For a focused executable example of the current core's behavior, run its scripted
integration tests:

```sh
cargo test --locked -p ion-core --test k5_generation
cargo test --locked -p ion-core --test k7_config
```

These exercise the libraries; they are not evidence of live-provider effectiveness
or a usable terminal application.

## Project documentation

- [DESIGN.md](DESIGN.md): accepted target architecture and invariants.
- [ROADMAP.md](ROADMAP.md): work order, implementation status and evidence.
- [TERMINAL.md](TERMINAL.md): target interaction and frontend requirements.
- [Source layout](docs/source-layout.md): crate and module ownership.
- [Research](docs/research.md): source comparisons and trade-offs.
- [AGENTS.md](AGENTS.md): repository working instructions.

Previous design and terminal acceptance records are preserved in
[history](docs/history/README.md). The `last-go` tag is historical recovery material,
not a migration target.

## License

[MIT](LICENSE)
