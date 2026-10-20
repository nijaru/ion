# Ion

Ion is an experimental Rust agent engine and coding application. Terminal chat,
headless runs, RPC clients and Rust embeddings share one agent loop and saved
SQLite Sessions. Coding tools can inspect and change files, run commands, manage
programs and delegate work. Everyday coding reliability is not yet qualified.

## Start

Use the stable toolchain selected by `rust-toolchain.toml`:

```sh
cargo build --locked -p ion
target/debug/ion models
```

Run `target/debug/ion` below, or install it on your `PATH`:

```sh
cargo install --locked --path crates/ion-app
```

Ion uses a matching provider API key from the environment when available. Use
`ion login PROVIDER` for masked key entry and `ion use PROVIDER MODEL` to choose
an exact model from `ion models`. [Configuration](docs/configuration.md) explains
default selection, custom endpoints, credentials and reasoning preferences.

From your project directory:

```sh
ion                              # inline terminal chat
ion run 'Inspect the failing test'
ion -p 'Summarize the changes'
git diff | ion -p 'Review this change'
ion --json run 'Inspect this project' > events.jsonl
ion rpc                          # persistent foreground JSONL control
ion --continue                   # reopen the latest Session here
ion sessions                     # list saved Sessions
ion --session ID inspect          # committed facts as JSON
ion --continue export             # readable transcript on stdout
```

Tools run with the host user's permissions. There is no implicit sandbox or
rollback. `--read-only` restricts model tools; it does not revoke separate,
explicit user-shell authority. Cancellation requests stop and joins owned work,
but does not undo effects or prove remote/escaped descendants stopped. Reopening
never silently replays an uncertain call. Inspect committed facts and actual
workspace effects before retrying interrupted work.

## Guides

| Need | Guide |
| --- | --- |
| Edit prompts, steer, queue follow-ups, navigate Sessions, inspect/copy/export output, attach images | [Terminal use](docs/terminal.md) |
| Select models/reasoning/tools; configure instructions, skills, templates, trusted commands and MCP | [Configuration and trust](docs/configuration.md) |
| Understand file tools, output retrieval, managed programs/subagents, questions, compaction and Code Mode | [Coding tools and context](docs/tools.md) |
| Script headless runs or implement an Ion RPC client | [Headless and RPC clients](docs/automation.md) |
| Assemble restricted or coding applications; manage admission, recovery, delegation and attachment | [Rust embedding](docs/embedding.md) |
| Understand accepted engine/application contracts, including unfinished capabilities | [Architecture](ARCHITECTURE.md) |

Guides describe delivered behavior. Architecture distinguishes accepted contracts
from open design choices; neither passing tests nor reference similarity proves
Pi/fx-level workflow quality.

## Current limits

Restricted Rust applications can use shared control, but the durable/service
contracts are not all implemented:

| Boundary | Implemented today | Not yet implemented |
| --- | --- | --- |
| Application setup | Caller-owned model/tool/instruction configuration, workspace-free Sessions and explicit shell/local-image authority. Opt-in continuation checks a saved authority reference and declarations. | Changed-authority reconciliation. Applications must reconstruct and attest live bindings on reopen; the reference is not a credential or sandbox. |
| Durability | SQLite conversation/effect history, conservative recovery, restart-safe direct admission through the Rust SDK and keyed RPC literal text; explicit same-Turn continuation through the Rust SDK. | Durable pending queues, wire-protocol delivery IDs and automatic restart/continuation. Coding CLI/TUI/RPC submissions do not opt in to continuation. |
| Delegation | Main coding preset exposes fresh/fork foreground and background Sessions with inherited route/reasoning, application-owned retained results, wait/status/cancel/acknowledge and joined original answers. SDK children use the same stable identity, complete environments and conservative recovery; Code Mode nested tools remain distinct. | Live-model/service-scale delegation qualification and native-app visual acceptance remain open. Model arguments never grant execution authority. |
| Native programs | Scoped pipe/PTY programs, stdin interaction, bounded raw capture and screen pages, terminal resize, retained wait/close/release and shared application cleanup; live human attachment lease with a Ctrl-] terminal view; child-job scope reclaimed before answer release. | Concurrent model/human input, shell environment capture, graphical terminal protocols and restart attachment. |
| Controls and trust | Model-loadout include/exclude selection, read-only model tools, MCP direct/deferred/hidden exposure, digest-bound project MCP/command trust, explicit named user commands and host-permission execution. | Per-call approval/consent, OS sandboxing, arbitrary extension handlers/hooks/UI contributions, MCP OAuth and per-tool exposure patterns. Loadout selection and trust are not security boundaries. |
| Remote use | Foreground stdio RPC, lossy progress and complete pull snapshots through shared control. A service embedding retains/drives the owner independently of connections. | No built-in authenticated network server, reliable activity replay or durable queue/metadata recovery. RPC EOF still stops/joins work. |

Common coding workflows are implemented, but Pi-level daily-driver reliability
and terminal usability are not yet qualified. Selected live coding tasks have
succeeded; longer workflows have also produced false summaries, inaccurate final
reports and fresh duplicate mutations. Mechanical compaction and absence of
harness replay do not establish truthful recall or once-only model behavior.
Check committed outcomes, workspace changes and native tests, not a recap alone.

CI exercises built headless, embedded, RPC and PTY workflows, including storage
faults, cancellation, recovery, resources and MCP. These checks do not establish
sustained repository work, representative native-emulator stability, clipboard
backend coverage or human visual usability. An earlier isolated Ghostty
qualification failure (exit 101, no useful trace) remains unresolved; PTY success
does not close it. Harness latency and memory bounds
need representative measurements; no comparative performance gain is claimed.

Direct Anthropic and OpenAI still need live account qualification; thinking on
custom llama.cpp routes is unsupported. Historical route success does not imply
current availability. Ion has no native ChatGPT subscription integration. Context
pressure uses approximate estimates, refined by compatible observed input usage
when available. Unknown-window routes rely on the encoded request bound. New image
sets still use a conservative visual-token allowance; calibration cannot prevent
an initial image request from exceeding that estimate.

Chat-compatible tool messages encode
`{"is_error": bool, "result": original_payload}`; inspection keeps raw payloads
and status separate. Saved OpenRouter structured reasoning from older tool-message
or withheld-output encodings is incompatible: compact that Session or start a new
one. Ion never silently rewrites a potentially signed prefix or discards reasoning.
See [ARCHITECTURE.md](ARCHITECTURE.md) for contracts and [AGENTS.md](AGENTS.md)
for repository checks.

## Development

Repository checks and source-owner navigation are in [AGENTS.md](AGENTS.md).
Documentation-only changes require link, command and consistency checks, not
claims of new runtime qualification.

## License

[MIT](LICENSE)
