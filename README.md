# Ion

Ion is a terminal coding agent: a small model-facing agent loop inside
a durable, single-writer session runtime.

This is a clean-sheet Rust rewrite at v0. The authoritative target
design is [DESIGN.md](DESIGN.md). The last Go implementation is tagged
`last-go` and is not a design, behavior, or migration reference.

## Quickstart

Requires a Rust stable toolchain.

```sh
cargo run -p ion
```

This opens the interactive TUI with the scripted provider — no model
key needed. To use a real model, set `OPENROUTER_API_KEY` and either
put `defaultModel` in `~/.config/ion/settings.toml` or pass it:

```sh
cargo run -p ion -- --model stealth/ox-alpha
```

## Usage

| Command | Behavior |
| :--- | :--- |
| `ion` | Interactive TUI (new session) |
| `ion --resume` | Reopen the most recent persisted session |
| `ion -p "prompt"` | Run one prompt in print mode and exit |
| `ion --acp` | Serve Agent Client Protocol v1 on stdio |
| `ion --allow bash,write` | Print mode: tools that may run without approval |
| `ion --trust-project` | Load project-local `.ion/extensions.toml` for this run |

In print mode everything else terminates the operation with an
approval requirement instead of executing. In the TUI, `/help` lists
the slash commands (`/compact`, `/model`); `/model <id>` switches
models durably at the next step boundary and survives restart.

Sessions persist to SQLite under `$XDG_DATA_HOME/ion/` (or the
platform default) and are replayed on resume; compaction, steering,
cancellation, and model selection survive restarts.

For opt-in terminal diagnosis, set `ION_TERMINAL_CAPTURE` to a file
path before launching the TUI. The capture contains emitted terminal
bytes, including rendered prompt text, so do not enable it for
untrusted or sensitive sessions.

## Configuration

Settings live at `~/.config/ion/settings.toml`. Minimal example:

```toml
theme = "dark"
defaultModel = "stealth/ox-alpha"
sandbox = "auto" # auto, unconfined, seatbelt, or bubblewrap

[[mcp_servers]]
name = "docs"
command = "npx"
args = ["-y", "@some/mcp-docs-server"]
```

Malformed settings are a hard error, never silently ignored.
`auto` selects Seatbelt on macOS or Bubblewrap on Linux when available;
explicit sandbox modes fail closed if their backend is unavailable.
Project-local extensions load only behind explicit `--trust-project`.

## Development

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

CI runs the lint gates only; tests are expected to pass locally before
push. Current work and status live in [AGENTS.md](AGENTS.md) and the
central `agent-context` brief.

## License

[MIT](LICENSE)
