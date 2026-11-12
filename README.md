# Ion

Ion is an **experimental Rust agent harness and coding application**. It shares one agent/session engine across headless CLI, terminal chat, stdio RPC and Rust embedding. Coding tools include files, search, guarded edits, native shell execution, web access and optional MCP/Code Mode. Sessions are stored in SQLite. The core is intended to support non-coding hosts as well.

The target is Pi-like coding capabilities and interaction, with compact fx-like terminal styling, implemented idiomatically in Rust rather than copying either implementation.

**Status:** unreleased v0. Everyday coding reliability and native terminal usability are not yet qualified; the TUI is not a recommended daily driver. Implemented behavior is defined by source, not planned features.

## Build

Use the toolchain in `rust-toolchain.toml`:

```sh
cargo build --locked -p ion
target/debug/ion models
```

Or install with `cargo install --locked --path crates/ion-cli`.

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

Terminal chat temporarily enables grapheme clustering (mode 2027) when the terminal reports it as supported and currently off, and restores the prior setting when yielding the terminal. Without reported support, rendering stays best-effort; complex emoji alignment depends on the terminal and its Unicode width tables.

Ctrl-C stops running work immediately, including unanswered questions, except while attached to a program, where it is forwarded to that program. An external editor handles its own Ctrl-C; cancelling it returns to Ion with the original draft retained. A pending clipboard read in the composer is cancelled first. Esc dismisses local UI; stopping via Esc, clearing a draft, cancelling a question with Esc, and quitting with a shortcut ask for confirmation. Reported key repeats cannot submit input, accept completions or choices, toggle tool output or question/picker controls, open an external editor, or start another cancellation; ordinary held-key editing and navigation still repeat. Legacy terminals cannot distinguish auto-repeat from another press.

Ctrl-O expands or collapses tool output without leaving the composer. Inline chat temporarily uses the shared fullscreen conversation to inspect expanded output, then returns to native scrollback when collapsed; published history is not rewritten. Expansion shows recorded directory entries, file-search results and remote-tool text or structured values; it cannot recover data omitted during capture. Use `/tool N` for raw tool evidence or `/tool all` for the raw conversation inspector.

The coding recipe runs native commands **with your host permissions**. Tool selection/read-only flags are model loadout controls, not a sandbox. Cancellation does not undo filesystem effects; interrupted effects may have unknown outcomes and are not blindly replayed.

Reopening a session does not restart its work. To continue after an interruption, send a new message in that session; it uses the current model, credentials and tools. Inspect unknown effects before asking to repeat a mutation. The previous Turn is recorded as interrupted and the new message starts a new Turn, with missing results marked unknown rather than rerun.

Compaction summarizes bounded excerpts; original result envelopes and retained captures remain in the Session's SQLite database. Native command overflow/raw bytes, omitted web snapshots (converted text, not raw HTML), and retained MCP responses/resources use typed artifact receipts. Capture completeness is separate from command or server success. Partial, unfinished and unclaimed captures are retained conservatively, not automatically deleted or replayed.

The coding `history` tool searches recorded evidence without rerunning effects. Its `artifact` action reads a referenced capture by source entry and artifact index; Code Mode uses `inspectSaved({kind:'artifact',parent,child,artifact:0})`, including committed children of the current invocation. Pages return readable UTF-8 text where possible, otherwise exact base64 bytes. Check `encoding`; `data_bytes` counts the page's raw bytes, while `total_bytes` is the full capture size. Consume the page's data even when `next_offset` is null (EOF); otherwise follow `next_offset`. Offsets and the maximum 16 KiB page limit count raw bytes. Text pages preserve character boundaries without replacing malformed bytes. Readers enforce the issuing request's frozen prefix and exclude private `!!` shell results. An ID, hash or look-alike JSON reference grants no access.

When a Code Mode script fails after admitting child calls, its error result includes `recovery.query`. Inside a later Code Mode call, pass that query to the global `inspectSaved` helper to inspect saved child outcomes before retrying. A script failure does not undo effects or require rerunning tools to recover their output.

Conversation clones, forks and delegated forks copy their referenced observed captures before publishing the new history; they do not copy unclaimed forensic data. A consistent whole-database backup can retain that data as well. Chunk commits add SQLite/WAL work and disk retention; there is no automatic artifact eviction.

During early unstable development, incompatible Session databases are rejected rather than migrated. Rust `ToolOutput` includes typed `artifacts`; observed shell outcomes retain the whole output. Low-level tool execution takes an explicit `ToolInvocation`; outside admitted execution, callers must supply an artifact sink to retain captures. These unreleased APIs have no compatibility shim.

Configuration, saved credentials, resources and sessions use `~/.ion` by default, or `ION_HOME` when set; workspace-specific `.ion` is separate. No automatic migration from old XDG storage is performed.

See [Sessions and alternative conversations](docs/sessions.md) for finding saved work, exploring context without changing files, and choosing clone or fork boundaries.

See [Skills, prompts and commands](docs/customization.md) to add project skills, reusable `/name` prompts and explicit `/command:name` executables. `/skills`, `/prompts` and `/commands` show loaded resources; `/reload` refreshes them and reports source warnings.

See [Delegating coding work](docs/delegation.md) for background subagents, full-handle `/tasks` lookup, progress, cancellation and retained results.

See [Long-running programs](docs/processes.md) for starting supervised programs, `/attach`, Ctrl-] detachment, inspection and stop/join controls. [Tool selection and operator vetoes](docs/tool-control.md) explains model loadouts, optional pre-dispatch refusal programs and their limits.

## Develop and embed

This workspace contains `ion-ai` (model-facing types), `ion-core` (Turn and Session runtime), `ion-app` (the shared application, SDK and effects), and `ion-terminal` (physical terminal). The `ion` executable and its CLI, TUI and RPC clients live in `crates/ion-cli`. These are current code owners, not promised permanent API boundaries.

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo build --locked -p ion
scripts/smoke.sh
```

Rust application entry points and examples live in `crates/ion-cli/examples/`. `CodingApplication` and `SessionControl` embed the same execution engine; APIs may change before release. `AGENTS.md` contains project-specific development checks.

`Session::create/open` and `SessionSetup::new` are blocking factory operations. Async coding clients use retained `CodingApplication::prepare_setup`, Session I/O and reader capabilities. Keep `setup.retirements()` outside the controller: after controller shutdown, drain reliable updates, release every Session/read capability, then await the retirement scope's `join()`. Shutdown preserves read-only access; retirement separately joins native SQLite close and writer-lease release. Cancelling a borrowed wait does not cancel or discard its native work.

A local input ID or pending launch is not durable acceptance. Use Core input/request receipts and reliable control updates to distinguish committed, not accepted and unresolved inputs. Never automatically retry or restore an unresolved input as a fresh request.

## Limitations

Durable storage and conservative effect recovery are implemented, but fully automatic service restart/wake, arbitrary dynamic extensions, provider coverage, broad ACP/editor integrations and model-backed daily-driver qualification are incomplete. Code Mode is optional in the current application; restart-safe guest reconstruction and full Pi Durable-level nested-call semantics are not yet established. Live model access and costs depend on the selected provider.

## License

MIT. See [LICENSE](LICENSE).
