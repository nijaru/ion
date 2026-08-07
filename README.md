# Ion

> [!NOTE]
> Ion is a native Go coding agent. The core submit/stream/tool/cancel/persist/
> resume path has deterministic, race, approved live OpenRouter, and current
> release-verification coverage; advanced integrations remain in progress.

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
Ion does not currently use OpenAI or Codex subscription OAuth credentials;
authentication is API-key based or supplied through an OpenAI-compatible
endpoint configuration.

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
| Alt+Up | Recall queued turns (deferred while runtime owns the queue) |
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
part of the default test run. The optional rich probe can exercise two
materially different provider adapters when those profiles are available; it
never accepts or persists API keys. One standards-compliant live provider path
is sufficient for the core gate, and additional-provider failures are targeted
compatibility work:

```sh
ION_LIVE_PROVIDER_A=openrouter \
ION_LIVE_MODEL_A=poolside/laguna-s-2.1:free \
ION_LIVE_PROVIDER_B=openai-compatible \
ION_LIVE_MODEL_B=qwen3.6:27b \
ION_LIVE_ENDPOINT_B=http://fedora:8080/v1 \
ION_LIVE_SMOKE=1 \
go test ./cmd/ion -run TestLiveSmokeTurnAndToolCall -count=1 -timeout 180s -v
```

The example uses a local Fedora Qwen endpoint for the second adapter. Run it
only when that endpoint and its credentials are available; it is separate from
the low-cost current-model basic smoke below.

For low-cost current-model verification, the basic smoke checks streamed text,
settlement, SQLite persistence, and replay. It is sufficient for the core live
provider path; use the richer probe only when a second adapter is available or
specific provider compatibility needs investigation:

```sh
ION_LIVE_PROVIDER_A=openrouter \
ION_LIVE_MODEL_A=poolside/laguna-s-2.1:free \
ION_LIVE_PROVIDER_B=openrouter \
ION_LIVE_MODEL_B=deepseek/deepseek-v4-flash-0731 \
ION_LIVE_BASIC=1 \
go test ./cmd/ion -run TestLiveBasicTurn -count=1 -timeout 120s -v
```

Set `ION_PHASE1_LIVE=1` with the same four profile variables to include the
live TUI/tmux pass in `scripts/smoke/phase1-acceptance.sh`.

Set `ION_LIVE_THINKING_A` or `ION_LIVE_THINKING_B` to an explicit thinking
level when a profile must prove reasoning deltas; the harness checks the
model capability and the streamed event. Set `ION_LIVE_REQUIRE_THINKING=1` to
require thinking deltas from both profiles. These checks still require the
opt-in live run and do not replace provider failure/cancellation evidence.

The failure probes are separate opt-in requests for targeted provider
compatibility checks. They require explicit profiles and are never enabled by
`ION_LIVE_SMOKE=1` alone:

```sh
ION_LIVE_AUTH_FAILURE=1 ION_LIVE_SMOKE=1 \
go test ./cmd/ion -run TestLiveProviderAuthenticationFailure -count=1 -timeout 90s -v

ION_LIVE_CANCELLATION=1 ION_LIVE_SMOKE=1 \
go test ./cmd/ion -run TestLiveProviderCancellation -count=1 -timeout 120s -v
```

The auth probe uses an invalid runtime-only token and reports only the
provider/status classification. The cancellation probe sends a real request,
then aborts at the response-body boundary and checks the aborted terminal event
plus no durable replay. Controlled adapter tests cover deterministic 429,
malformed, and overflow behavior separately; none of these probes replaces the
provider-specific regression evidence.

## License

[MIT](LICENSE)
