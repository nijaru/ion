# ion

Fast, lightweight, open-source coding agent.

> **Note:** ion is in early development. Expect breaking changes.

## Features

- Multi-provider LLM support (OpenRouter, Anthropic, Ollama)
- Built-in tools: read, write, edit, grep, glob, bash
- MCP support

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

Config file: `~/.config/ion/config.toml`

```toml
openrouter_api_key = "sk-or-..."
model = "anthropic/claude-sonnet-4"
```

## License

[MIT](LICENSE)
