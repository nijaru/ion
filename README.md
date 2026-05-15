# ion

A terminal coding agent for local repositories.

> Early preview. Ion is still changing, and it can run shell commands and edit files in the repository where you launch it. Review changes before committing them.

Ion runs in your terminal, talks to an LLM provider, and gives the model a small set of coding tools for reading, searching, editing, and running commands in the current workspace.

It is written in Go and uses a native provider path for API-backed models. The focus right now is the basic coding loop: submit a prompt, stream the response, call tools, edit files, cancel cleanly, and resume the session later.

## Features

- Interactive terminal UI with multiline input
- One-shot print mode for scripts and smoke tests
- Session persistence with resume and continue
- Provider and model switching from the TUI
- File tools for read, write, edit, multi-edit, list, grep, and glob
- Foreground shell command execution
- OpenAI, Anthropic, Gemini, OpenRouter, Ollama, local API, and OpenAI-compatible providers

## Installation

Ion requires Go 1.26 or newer.

```sh
go install github.com/nijaru/ion/cmd/ion@latest
```

From a local checkout:

```sh
go install ./cmd/ion
```

Make sure your Go binary directory is on `PATH`:

```sh
export PATH="$(go env GOPATH)/bin:$PATH"
```

## Configuration

Stable settings live in `~/.ion/config.toml`. Runtime choices made inside the TUI are stored in `~/.ion/state.toml`.

Create `~/.ion/config.toml`:

```toml
provider = "openrouter"
model = "openai/gpt-5.4"
reasoning_effort = "auto"
```

Set the matching API key:

```sh
export OPENROUTER_API_KEY="..."
```

Local OpenAI-compatible server example:

```toml
provider = "local-api"
model = "qwen3.6:27b"
endpoint = "http://localhost:8080/v1"
context_limit = 70000
```

Temporary runtime overrides:

```sh
ION_PROVIDER=openrouter ION_MODEL=openai/gpt-5.4 ion
ion --provider local-api --model qwen3.6:27b
```

## Usage

Start the interactive TUI:

```sh
ion
```

Run one prompt and exit:

```sh
ion -p "summarize the current repo"
ion --continue -p "what did we do last?"
ion -p --output json "reply with ok"
```

Common TUI commands:

```text
/help       show commands and keys
/provider   choose a provider
/model      choose a model
/thinking   choose reasoning effort
/tools      show available tools
/status     show runtime status
/resume     resume a previous session
/compact    compact the current session
/quit       exit
```

## Security

Ion currently trusts local tool execution by default. The model can read files, edit files, and run foreground shell commands in the workspace.

Use it in repositories where that is acceptable, and review the diff before committing. Approval prompts, persistent permission policy, and richer sandbox UX are planned after the core agent loop is stable.

## Development

```sh
go test ./...
go vet ./...
go run ./cmd/ion
```

## License

[MIT](LICENSE)
