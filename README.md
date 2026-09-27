# Ion

Ion is an unreleased Rust coding agent for a local working directory. It has
one coding loop for terminal chat, headless prompts and library hosts. The
agent can read, edit and write files and run a native shell command. Sessions
are saved in SQLite and continue across launches.

## Start

Build with the checked-in Rust 1.98.0 toolchain:

```sh
cargo build --locked -p ion
target/debug/ion models
```

Ion picks a catalog model automatically when its provider key is present in
`DEEPSEEK_API_KEY`, `XIAOMI_API_KEY`, `OPENROUTER_API_KEY`, `OPENAI_API_KEY` or
`ANTHROPIC_API_KEY`. The default preference starts with DeepSeek Flash, then
MiMo Flash, then DeepSeek Flash through OpenRouter. To select one
explicitly, run `ion use PROVIDER MODEL` with the exact ID shown by `ion
models`. `ion login openrouter` opens OpenRouter's browser login and saves the
returned key locally. `ion login-key PROVIDER` accepts a key at a masked
terminal prompt; `ion auth` shows which credential source is active, and
`ion logout PROVIDER` removes a saved key. Environment keys take precedence.
With the existing Pi FNOX profile, for example:

```sh
fnox --profile pi exec -- target/debug/ion --provider deepseek --model deepseek-flash
```

From the project directory:

```sh
ion                        # terminal chat
ion run 'Inspect and fix the failing test'
ion -p 'Summarize the changes'
ion inspect                # committed session history as JSON
```

`--cwd PATH` chooses a working directory; `--session PATH` selects an explicit
SQLite session. By default, Ion reopens the session associated with the
current directory. It loads `AGENTS.md` instructions found along that
directory's ancestor path. `ion use` also accepts a custom model with
`--endpoint URL --wire chat-completions` or `--wire anthropic-messages`;
custom remote endpoints require HTTPS and a key supplied through
`ION_CUSTOM_API_KEY` or `--api-key-env NAME`. Literal loopback HTTP can run
without a key.

Tools act directly in the working directory with the host user's permissions.
There is no implicit sandbox. If a process stops during a tool call, Ion
records its effect as unknown when the next prompt begins; it does not rerun
the call automatically. `ion inspect` reads the existing log without making
that repair. Current sessions use the full recorded conversation until the
request size limit is reached; automatic compaction is not implemented.

## Status and development

The built headless and terminal clients have completed offline coding tasks
with a deterministic local model stream: read, edit, write, shell, and reopen.
A live DeepSeek V4.1 Flash task exercised read, edit, write, shell and headless
resume using the Pi FNOX profile. MiMo V2.6 Flash and DeepSeek V4.1 Flash
through OpenRouter completed separate live coding tasks. OpenRouter GPT-5.4
also completed headless and terminal tasks, but the cheaper Flash routes are
the default preference. Resulting files and committed sessions were checked
independently. Direct OpenAI returned HTTP 429. Anthropic and other
model/account combinations remain unqualified here. OpenRouter browser login
has focused tests; a live login attempt timed out without a callback.
[ARCHITECTURE.md](ARCHITECTURE.md) holds the design contracts.

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
