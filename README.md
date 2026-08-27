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

This uses the compiled-in Pi-aligned default, `openai-codex/gpt-5.6-luna`.
Ion reads an existing Pi OAuth credential from `~/.pi/agent/auth.json`
without modifying it. To use OpenRouter instead, set
`OPENROUTER_API_KEY` and pass a provider/model reference:

```sh
cargo run -p ion -- --model openrouter/stealth/ox-alpha
```

For an explicit Codex credential, set `OPENAI_CODEX_ACCESS_TOKEN` and
`OPENAI_CODEX_ACCOUNT_ID`. Ion does not refresh or rewrite Pi credentials.

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
the slash commands (`/compact`, `/model`); `/model` lists the host-provided
catalog and `/model <id|number>` switches models durably at the next step
boundary and survives restart. `ctrl+j` inserts a newline; Tab completes only
known slash commands and catalog models.

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
defaultProvider = "openai-codex"
defaultModel = "gpt-5.6-luna"
# Optional finite list for the TUI's `/model` selector.
modelCatalog = ["gpt-5.6-luna", "gpt-5.6-sol"]
defaultThinkingLevel = "xhigh"
sandbox = "auto" # auto, unconfined, seatbelt, or bubblewrap

[[mcpServers]]
name = "docs"
command = "npx"
args = ["-y", "@some/mcp-docs-server"]

# Keep the model-facing MCP set explicit and small.
activeMcpServers = ["docs"]
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
