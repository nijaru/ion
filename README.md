# Ion

> [!NOTE]
> Ion is a native Go coding agent. The core submit/stream/tool/cancel/persist/
> resume path has deterministic and race coverage, while end-to-end live
> provider, release, and some advanced integration gates remain in progress.

Ion is a terminal coding agent for working on codebases from your shell. It
opens an interactive chat UI, gives the model a small set of coding tools, and
keeps sessions available so you can resume work later.

## Install

Ion requires Go 1.26 or newer.

From a local checkout:

```sh
go install ./cmd/ion
```

From GitHub:

```sh
go install github.com/nijaru/ion/cmd/ion@latest
```

Make sure Go's binary directory is on your `PATH`:

```sh
export PATH="$(go env GOPATH)/bin:$PATH"
```

Then start Ion:

```sh
ion
```

## Provider Setup

The first interactive run opens the provider picker. Choose a provider, enter
an API key when prompted, then choose a model. Providers that do not expose a
model catalog open a model-ID prompt instead; enter the exact ID expected by
that provider. You can also use `/model <id>` at any time.

Ion supports direct API-key providers and custom OpenAI-compatible endpoints.
OpenAI-compatible `/v1` APIs are the usual interface for local model servers,
hosted gateways, and self-managed inference services.

Most users do not need a config file. Use `~/.ion/config.toml` only for custom
endpoints, stable defaults, or provider settings you want outside the TUI.

Example custom/local endpoint:

```toml
# ~/.ion/config.toml
provider = "openai-compatible"
model = "qwen3.6:27b"
endpoint = "http://localhost:11434/v1"
context_limit = 70000
```

Example endpoint with an environment-backed token:

```toml
# ~/.ion/config.toml
provider = "openai-compatible"
model = "provider/model"
endpoint = "https://example.com/v1"
auth_env_var = "CUSTOM_API_KEY"
```

Runtime choices made in the TUI are stored in `~/.ion/state.toml`. API keys
entered in Ion are stored in `~/.ion/credentials.toml`.

Configured MCP stdio servers are declared in `~/.ion/config.toml` with
`[[mcp_servers]]` entries. See [docs/tools.md](docs/tools.md#mcp-servers) for
the server lifecycle, namespacing, workspace policy, and approval behavior.

Explicit workspace memory is available through `/memory`. To expose the
model-visible `recall_memory` and `remember_memory` tools, set
`memory_tools = "on"`; notes remain separate from session history and are never
injected into prompts automatically. See [docs/memory.md](docs/memory.md).

Per-run overrides are also supported:

```sh
ION_PROVIDER=openai ION_MODEL=gpt-5.5 ion
ion --provider openai-compatible --model qwen3.6:27b
```

## Usage

Start the TUI:

```sh
ion
```

Inspect the installed build without loading configuration or opening storage:

```sh
ion --version
ion --help
```

Run a prompt and print the answer:

```sh
ion -p "summarize this project"
cat README.md | ion -p "summarize this"
ion --continue -p "what did we do last?"
ion --json -p "reply with ok"
ion --output events -p "run the checks"
```

Print mode uses the same runtime as the TUI. `--output text` streams the
assistant response, `--output json` emits one final result summary, and
`--output events` emits newline-delimited, versioned Ion lifecycle events plus
a final result record. The events format is Ion-owned and is not a Pi session
or JSONL compatibility protocol.

Common TUI commands:

```text
/help       show commands and keys
/hotkeys    show all keyboard shortcuts
/provider   choose a provider
/login      save an API key
/model      choose a model
/thinking   choose reasoning effort
/status     show runtime status
/jobs       list or stop background shell jobs
/rewind     preview or restore a workspace checkpoint
/resume     resume a previous session
/compact    compact the current session
/clone      duplicate the current session
/copy       copy last assistant response to clipboard
/export     export session as JSON bundle
/export-html  export session as self-contained HTML
/import     import session from JSON bundle
/tree       show session lineage and children
/name       set session display name
/logout     clear provider API key
/reload     reload keybindings and the active runtime configuration
/scoped-models  show configured scoped models
/changelog   show changelog entries
/debug       write debug diagnostics to log file
/quit       exit
```

## Hotkeys

| Key | Action |
|-----|--------|
| Ctrl+L | Cycle model forward (scoped models or primary/fast) |
| Ctrl+Shift+L | Cycle model backward |
| Ctrl+G | Open external editor |
| Ctrl+T | Toggle thinking blocks visibility |
| Ctrl+O | Toggle tool output |
| Shift+Tab | Cycle thinking level |
| Alt+Up | Recall queued turns |
| Alt+Enter | Queue follow-up |
| Ctrl+C | Clear editor (double-tap to quit) |
| Ctrl+D | Exit (double-tap when empty) |
| Ctrl+Z | Suspend |
| Ctrl+V | Paste image |
| Up/Down | History navigation |
| Enter | Send message |
| Ctrl+J / Shift+Enter | Insert newline |

## Development

```sh
go run ./cmd/ion
go test ./...
go vet ./...
scripts/smoke/tmux-minimal-harness.sh
```

Live provider smoke tests are gated behind environment variables and are not
part of the default test run.

## License

[MIT](LICENSE)
