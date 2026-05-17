# Ion

> [!NOTE]
> Ion is early preview software. It can edit files and run shell commands in the
> current workspace.

Ion is a terminal coding agent for local development. It runs in your shell,
connects to your model provider, keeps sessions on disk, and can inspect code,
edit files, and run project commands.

## Quickstart

Ion requires Go 1.26 or newer. Install it with Go:

```sh
go install github.com/nijaru/ion/cmd/ion@latest
```

Make sure your Go binary directory is on `PATH`:

```sh
export PATH="$(go env GOPATH)/bin:$PATH"
```

Start Ion in a project:

```sh
ion
```

Ion opens a provider picker the first time it starts without a configured
runtime. Choose a provider, enter an API key when prompted, then choose a model.

## Providers

Ion supports API-key providers such as OpenAI, Anthropic, Gemini, OpenRouter,
and several OpenAI-compatible services. It also supports local model servers
that expose an OpenAI-compatible `/v1` API.

Most users do not need a config file. Use `~/.ion/config.toml` for custom
endpoints, stable defaults, or provider options you want to keep outside the
TUI.

Use `openai-compatible` for local or custom OpenAI-compatible endpoints. API
keys are optional for this provider unless your endpoint requires one:

```toml
# ~/.ion/config.toml
provider = "openai-compatible"
model = "qwen3.6:27b"
endpoint = "http://localhost:11434/v1"
context_limit = 70000
```

For a custom endpoint that requires an environment-backed token:

```toml
# ~/.ion/config.toml
provider = "openai-compatible"
model = "provider/model"
endpoint = "https://example.com/v1"
auth_env_var = "CUSTOM_API_KEY"
```

Runtime selections made in the TUI are stored in `~/.ion/state.toml`. API keys
entered in Ion are stored in `~/.ion/credentials.toml`.

You can override the config for a single run:

```sh
ION_PROVIDER=openai ION_MODEL=gpt-5.5 ion
ion --provider openai-compatible --model qwen3.6:27b
```

## Usage

Start the TUI:

```sh
ion
```

Run a non-interactive prompt:

```sh
ion -p "summarize this project"
cat README.md | ion -p "summarize this"
ion --continue -p "what did we do last?"
ion -p --json "reply with ok"
```

Common TUI commands:

```text
/help       show commands and keys
/provider   choose a provider
/login      save a provider API key
/model      choose a model
/thinking   choose reasoning effort
/status     show runtime status
/resume     resume a previous session
/compact    compact the current session
/quit       exit
```

## Development

Use the standard Go toolchain:

```sh
go install ./cmd/ion
go run ./cmd/ion
go test ./...
go vet ./...
```

Live provider smoke tests are gated behind environment variables and are not
part of the default test run.

## License

[MIT](LICENSE)
