# Ion

> [!NOTE]
> Ion is an early preview. It can read, edit, and run commands in your working
> tree, but the core agent and TUI are still being hardened.

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
an API key when prompted, then choose a model.

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

Run a prompt and print the answer:

```sh
ion -p "summarize this project"
cat README.md | ion -p "summarize this"
ion --continue -p "what did we do last?"
ion --json -p "reply with ok"
```

Common TUI commands:

```text
/help       show commands and keys
/provider   choose a provider
/login      save an API key
/model      choose a model
/thinking   choose reasoning effort
/status     show runtime status
/resume     resume a previous session
/compact    compact the current session
/quit       exit
```

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
