# Ion

Ion is an experimental Rust agent engine and coding application. One agent/Session engine serves headless CLI, terminal chat, stdio RPC and Rust hosts, including non-coding applications. The coding application provides filesystem, search, guarded editing, shell and web tools, with optional MCP and Code Mode. Conversations are saved in SQLite.

**Status:** unreleased v0. Everyday coding reliability and native terminal usability are not yet qualified; the TUI is not a recommended daily driver. Ion targets Pi-like capabilities and interaction with compact fx-like terminal styling. Source defines implemented behavior, not planned features.

## Build or install

Use the toolchain in `rust-toolchain.toml`:

```sh
cargo build --locked -p ion
target/debug/ion --help
```

To install `ion` on your Cargo binary path:

```sh
cargo install --locked --path crates/ion-cli
```

The examples below use the installed `ion`. For a build-only checkout, replace it with `target/debug/ion` when running from the repository root.

## Set up a model

First inspect the local model catalog and credential sources:

```sh
ion models
ion auth
```

Choose a provider ID and model ID from the catalog that you are authorized to use. Replace `PROVIDER` and `MODEL` below; catalog inclusion does not guarantee account access.

```sh
ion use PROVIDER MODEL
ion login PROVIDER
ion doctor
```

`login` takes an API key at a masked terminal prompt. Skip it if the route already has credentials through its supported environment variable or needs no key. `auth` shows sources without displaying secrets; `doctor` checks configuration offline. These setup commands do not send model requests. For a custom compatible endpoint, see `ion use --help`; supply the endpoint and wire format for your actual service rather than a catalog route.

For example, `ion use openai gpt-6.2-sol` selects the OpenAI Responses API with `OPENAI_API_KEY` or an API key saved with `ion login openai`. It does not use a ChatGPT subscription login.

`ion use google gemini-4-hydrogen` selects Gemini's GenerateContent API. Set `GEMINI_API_KEY` or run `ion login google` with a Gemini API key. Ion keeps the conversation in its Session and sends the history with each request. Long decode is not supported. For a custom route, `--wire gemini-generate-content` takes an API base URL (such as `https://generativelanguage.googleapis.com/v1beta`), not a model method URL. Custom routes must follow the same signed tool-call contract.

Configuration, saved credentials, resources and Sessions use `~/.ion` by default, or `ION_HOME` when set. Workspace `.ion` files are separate. Ion does not automatically migrate old XDG storage.

## Use

After configuration, submit a request or open terminal chat. Model requests can incur provider charges:

```sh
ion --read-only run 'Summarize this project'
ion                                  # experimental terminal chat
ion --continue                       # reopen latest Session in this directory
ion -r                               # choose a saved Session
```

`ion run 'PROMPT'` runs headlessly; `ion -p 'PROMPT'` is shorthand. Use `ion --json run 'PROMPT'` for JSONL events or `ion rpc` for stdio JSONL control. Headless and RPC clients require a configured model and credentials. CLI options can precede a command but must apply to it; use `ion --help` and `ion COMMAND --help` for exact options. In chat, `/help` lists controls and `/help keys` shows effective bindings.

**Permissions and recovery:**

- Coding commands run with your host permissions. Tool filters and `--read-only` restrict the model's loadout, not the host: explicit user shell and trusted executable commands remain available. They are not a sandbox or a per-call approval system.
- Ctrl-C immediately **requests** cancellation of foreground work, including unanswered questions; it may wait for cleanup to join. While attached to a program, Ctrl-C goes to that program instead. See [terminal controls](docs/terminal.md).
- Cancellation does not undo effects. Reopening does not restart interrupted work, and navigating, cloning or forking a conversation does not roll back files. Inspect unknown outcomes before asking to repeat a mutation.

## Documentation

- [Sessions and alternative conversations](docs/sessions.md): reopen, navigate, clone/fork, attach input, estimate context, compact and retrieve retained evidence.
- [Terminal chat](docs/terminal.md): composer controls, tool-output inspection, external editors and shutdown.
- [Tool selection and operator vetoes](docs/tool-control.md): model loadouts, native path rules, Code Mode recovery and optional refusal programs.
- [MCP tools](docs/mcp.md): connect local or HTTP servers, defer tool declarations and set up Context7.
- [Long-running programs](docs/processes.md): supervised programs, PTY attachment and stop/join controls.
- [Skills, prompts and commands](docs/customization.md): resource discovery, `/reload` and trusted executable commands.
- [Delegating coding work](docs/delegation.md): subagents, task controls and recorded results.
- [Headless JSONL and stdio RPC](docs/rpc.md): typed attachments and provisional streaming contracts.
- [Rust embedding](docs/embedding.md): entry points, input custody, cancellation and Session retirement.

## Develop

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo build --locked -p ion
scripts/smoke.sh
```

The smoke script exercises an offline built headless/reopen path. See `AGENTS.md` for project-specific development guidance and [Rust embedding](docs/embedding.md) for library examples and API documentation.

## Limitations

Durable storage and conservative effect recovery are implemented, but fully automatic service restart/wake, arbitrary dynamic extensions, provider coverage, and model-backed daily-driver qualification are incomplete. [ACP interoperability](docs/embedding.md#host-and-protocol-boundaries) is planned, not implemented. Code Mode is optional; restart-safe guest reconstruction and full Pi Durable-level nested-call semantics are not yet established. Incompatible Session databases are rejected rather than migrated during unstable development. Rust APIs may change before release.

## License

MIT. See [LICENSE](LICENSE).
