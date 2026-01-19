# ion

Fast, lightweight, open-source coding agent.

> **Note:** ion is in early development. Expect bugs, incomplete features, and breaking changes.

## Features

- Multi-provider LLM support (Anthropic, Google, Groq, Ollama, OpenAI, OpenRouter)
- Built-in tools: read, write, edit, grep, glob, bash
- MCP server support
- Session persistence

## Installation

```sh
cargo install --git https://github.com/nijaru/ion
```

## Usage

Interactive mode:

```sh
ion
```

One-shot mode:

```sh
ion run "explain this codebase"
```

## Configuration

Config locations:

- User: `~/.ion/config.toml`
- Project: `.ion/config.toml`
- Local (gitignored): `.ion/config.local.toml`

```toml
provider = "anthropic"
model = "claude-sonnet-4"
```

API keys via environment variables:

- `ANTHROPIC_API_KEY`
- `OPENROUTER_API_KEY`
- `GOOGLE_API_KEY`
- `OPENAI_API_KEY`
- `GROQ_API_KEY`

## Agent Instructions

ion reads `AGENTS.md` (or `CLAUDE.md` as fallback) from project root and user directories.

## License

[AGPL-3.0](LICENSE)
