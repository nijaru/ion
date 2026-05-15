# ion

> **Warning:** `ion` is work in progress and not stable yet. Expect bugs and breaking changes while the core agent loop and TUI are still being hardened.

`ion` is a terminal coding agent written in Go. Run it inside a repository, chat with a model, and let it inspect files, edit code, search, and run shell commands.

The current goal is a small, reliable coding-agent core before larger workflow features. Think Pi-style simplicity first: prompt, stream, tool call, edit, cancel, resume.

## What Works Today

- Interactive terminal UI with a multiline composer
- Native API-backed providers such as OpenAI, Anthropic, OpenRouter, Gemini, Ollama, local OpenAI-compatible servers, and other OpenAI-compatible APIs
- File and shell tools: `bash`, `read`, `write`, `edit`, `multi_edit`, `list`, `grep`, `glob`
- Session persistence, resume, continue, and one-shot print mode
- Model and provider switching from the TUI
- Local skill discovery with `/skills`

ACP/subscription bridges, background jobs, richer permission modes, remote sandboxes, and larger workflow features are intentionally deferred until the native core is stable.

## Install

Requires Go 1.26 or newer.

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

## Configure

`ion` reads stable user settings from `~/.ion/config.toml`.

Example using OpenRouter:

```toml
provider = "openrouter"
model = "openai/gpt-5.4"
reasoning_effort = "auto"
```

Then set the matching API key:

```sh
export OPENROUTER_API_KEY="..."
```

Example using a local OpenAI-compatible server:

```toml
provider = "local-api"
model = "qwen3.6:27b"
endpoint = "http://localhost:8080/v1"
context_limit = 70000
```

You can also override the runtime per command:

```sh
ION_PROVIDER=openrouter ION_MODEL=openai/gpt-5.4 ion
ion --provider local-api --model qwen3.6:27b
```

Provider/model choices made inside the TUI are stored in `~/.ion/state.toml`.

## Use

Start the TUI in a project:

```sh
cd path/to/repo
ion
```

Useful commands inside the TUI:

```text
/help       show commands and keys
/provider   choose a provider
/model      choose a model
/thinking   choose reasoning effort
/tools      show the current tool surface
/status     show runtime and safety posture
/resume     pick or resume a session
/compact    compact the current session
/quit       exit
```

One-shot print mode is useful for scripts and smoke tests:

```sh
ion -p "summarize the current repo"
ion --continue -p "what did we do last?"
ion -p --output json "reply with ok"
```

## Safety

The native path currently trusts local tool execution by default. That matches the current Pi-parity stabilization target, but it means model-driven tools can read and edit files and run foreground shell commands in the workspace.

Use `ion` only in repositories where you are comfortable letting the agent operate. Richer approvals, persistent permission policy, and sandbox UX are planned later after the core loop is reliable.

## Development

```sh
go test ./...
go vet ./...
go run ./cmd/ion
```

The active implementation is Go. The old Rust implementation is preserved under `archive/rust/` for reference only. The `stable-rnk` tag points at the last known stable Rust-era checkpoint.

Project planning and audit notes live in local `ai/` context files. Start with `ai/STATUS.md` and `ai/PLAN.md` when working on the repo.

## License

[MIT](LICENSE)
