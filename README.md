# Ion

Ion is an **experimental Rust agent harness and coding application**. It shares one agent/session engine across headless CLI, terminal chat, stdio RPC and Rust embedding. Coding tools include files, search, guarded edits, native shell execution, web access and optional MCP/Code Mode. Sessions are stored in SQLite. The core is intended to support non-coding hosts as well.

**Status:** unreleased v0. Everyday coding reliability and native terminal usability are not yet qualified; the TUI is not a recommended daily driver. Implemented behavior is defined by source, not planned features.

## Build

Use the toolchain in `rust-toolchain.toml`:

```sh
cargo build --locked -p ion
target/debug/ion models
```

Or install with `cargo install --locked --path crates/ion-app`.

## Basic usage

```sh
ion                                  # interactive terminal chat (experimental)
ion run 'Inspect and fix a failing test'
ion -p 'Summarize the changes'
ion --json run 'Review this project'  # machine-readable events
ion rpc                              # stdio JSONL control
ion models                           # available models
ion use PROVIDER MODEL               # select default model
ion login PROVIDER                   # masked API key entry
ion auth                             # credential sources
ion doctor                           # offline configuration diagnostics
ion --continue                       # resume latest session in this directory
ion -r                               # choose a saved session
ion sessions                         # list saved sessions
ion --continue turns                 # recorded turns
ion --continue usage                 # recorded token usage
ion --continue compact               # request context summary
```

Run `ion --help` and `ion COMMAND --help` for complete supported options. The current CLI supports `@file` context arguments, images, explicit model and reasoning selection, and tool include/exclude filters. Headless/RPC mode needs an already configured model and credentials.

The coding recipe runs native commands **with your host permissions**. Tool selection/read-only flags are model loadout controls, not a sandbox. Cancellation does not undo filesystem effects; interrupted effects may have unknown outcomes and are not blindly replayed.

Configuration, saved credentials, resources and sessions use `~/.ion` by default, or `ION_HOME` when set; workspace-specific `.ion` is separate. No automatic migration from old XDG storage is performed.

## Develop and embed

This is a Cargo workspace with `ion-ai` (model-facing types), `ion-core` (Turn and Session runtime), `ion-host` (applications and effects), `ion-terminal` (physical terminal) and `ion-app` (CLI and clients). These are current code owners, not promised permanent API boundaries.

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo build --locked -p ion
scripts/smoke.sh
```

Rust host entry points and examples live in `crates/ion-app/examples/`. The current `CodingApplication` and `SessionControl` APIs permit embedding the same execution engine; APIs may change before release. `AGENTS.md` contains project-specific development checks.

## Limitations

Durable storage and conservative effect recovery are implemented, but fully automatic service restart/wake, arbitrary dynamic extensions, provider coverage, broad ACP/editor integrations and model-backed daily-driver qualification are incomplete. Code Mode is optional in the current application; restart-safe guest reconstruction and full Pi Durable-level nested-call semantics are not yet established. Live model access and costs depend on the selected provider.

## License

MIT. See [LICENSE](LICENSE).
