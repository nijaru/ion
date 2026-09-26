# Ion

Ion is an unreleased Rust coding agent under active rewrite. The intended
product is a Pi-like local agent with read, edit, write and shell tools, a
terminal UI and headless mode, resumable sessions, and usable model selection
and login. [ARCHITECTURE.md](ARCHITECTURE.md) states the current design target
under review.

## Current status

The worktree is transitional. A previous mock-provider headless run exercised
native shell and file tools, model/tool continuation and session reopen, but
subsequent changes have not passed the full workspace build or live-model and
real-terminal coding checks. Do not treat this checkout as a completed coding
agent. The maintained code has OpenAI-compatible Chat Completions and Anthropic
Messages transports; the model catalog and interactive login are incomplete.
The excluded former `crates/ion` implementation is not a supported fallback.

The current CLI may require explicit provider configuration and model capacity
values. Its interface is being revised with the session/provider rewrite;
`ion --help` and `ion configure --help` are the source for the current built
binary once it compiles. A catalog model should eventually work without
manual capacity assertions, and ambient API keys should work without login.

## Development

Build with the checked-in Rust 1.98.0 toolchain:

```sh
cargo build --locked -p ion
```

Repository checks are:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
scripts/smoke.sh
```

The smoke script exercises the offline headless submit/reopen path. It does not
qualify a live provider or the terminal. The actual coding loop needs real
headless and terminal tasks before the first usability claim. See
[AGENTS.md](AGENTS.md) for maintainer instructions.

## License

[MIT](LICENSE)
