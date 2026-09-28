# Ion

Ion is an unreleased Rust coding agent for a local working directory. It has
one coding loop for terminal chat, headless prompts and library hosts. The
agent can read, edit and write files and run a native shell command. Sessions
are saved in SQLite and continue across launches. Ion is usable for short
coding tasks; its session and model controls are still simpler than Pi's.

## Start

Build with the checked-in Rust 1.98.0 toolchain:

```sh
cargo build --locked -p ion
target/debug/ion models
```

Use `target/debug/ion` in the commands below, or install the binary on your
`PATH` with `cargo install --locked --path crates/ion-app`.

Ion picks a catalog model automatically when its provider key is present in
`DEEPSEEK_API_KEY`, `XIAOMI_API_KEY`, `OPENROUTER_API_KEY`, `OPENAI_API_KEY` or
`ANTHROPIC_API_KEY`. The default preference starts with DeepSeek Flash, then
MiMo Flash, then DeepSeek Flash through OpenRouter. To select one explicitly,
run `ion use PROVIDER MODEL` with the exact ID shown by `ion models`.
`ion login PROVIDER` accepts a key at a masked terminal prompt when no
environment key is available. `ion auth` shows which credential source is
active, and `ion logout PROVIDER` removes a saved key. Environment keys take
precedence.
With the existing Pi FNOX profile, for example:

```sh
fnox --profile pi exec -- target/debug/ion --provider openrouter --model deepseek/deepseek-v4.1-flash
```

From the project directory:

```sh
ion                        # terminal chat
ion run 'Inspect and fix the failing test'
ion -p 'Summarize the changes'
ion --continue             # reopen the latest session in this directory
ion sessions               # list saved sessions and their IDs
ion --session ID inspect   # committed history for a selected session as JSON
```

`--cwd PATH` chooses a working directory. By default, a run starts a new
session; `--continue` reopens the most recently active one in that directory.
`--session ID` selects a listed session, and `--session PATH` can create or
open an exact SQLite path for scripts. An existing session uses its recorded
directory, and an explicit `--cwd` must match it. Headless runs print the
session ID to stderr. In the TUI, `/new`, `/resume`, `/name`, `/session` and
`/model` manage the conversation; `/login PROVIDER` and `/logout PROVIDER`
manage saved keys. The TUI model picker searches catalog and configured
custom routes. A resumed session restores its model; `ion use` sets the
default for new sessions.

Ion loads `AGENTS.md` instructions found along the working directory's
ancestor path. `ion use` also accepts a custom model with
`--endpoint URL --wire chat-completions` or `--wire anthropic-messages`.
For a llama.cpp server whose model emits unreplayable reasoning, use
`--wire llama-cpp-no-thinking` to disable it on each request. Custom remote
endpoints require HTTPS and a key supplied through
`ION_CUSTOM_API_KEY` or `--api-key-env NAME`. Literal loopback HTTP can run
without a key.

Tools act directly in the working directory with the host user's permissions.
There is no implicit sandbox. If a process stops during a tool call, Ion
records its effect as unknown when the next prompt begins; it does not rerun
the call automatically. `ion --continue inspect` reads the existing log
without making that repair. Current sessions use the full recorded
conversation until the request size limit is reached; automatic compaction
is not implemented.

## Current limits

Ion has completed live coding repairs with DeepSeek V4.1 Flash, MiMo V2.6
Flash, OpenRouter DeepSeek V4.1 Flash, and a custom llama.cpp endpoint reached
through a loopback tunnel on macOS. The code and session results were checked
independently; this is evidence for short tasks, not broad model or platform
parity.

Long histories end with an explicit request-size error because compaction is
not implemented yet. The TUI does not yet accept steering or follow-up input
while a turn runs, and tool output is clipped in its default view. Direct
OpenAI was rate-limited in the available account; Anthropic and live Linux
coding were not qualified. [ARCHITECTURE.md](ARCHITECTURE.md) holds the design
contracts.

Repository checks:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
scripts/smoke.sh
python3 scripts/smoke_terminal.py
```

The smoke checks use an offline local model stream. See [AGENTS.md](AGENTS.md)
for maintainer instructions.

## License

[MIT](LICENSE)
