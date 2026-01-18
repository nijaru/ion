# ion

Fast, lightweight, open-source coding agent.

## Features

- Multi-provider LLM support (OpenRouter, Anthropic, Ollama)
- Built-in tools: read, write, edit, grep, glob, bash
- Extensible via MCP servers

## Installation

```
cargo install --git https://github.com/nijaru/ion
```

Requires Rust nightly.

## Usage

Interactive mode:

```
ion
```

One-shot mode:

```
ion run "explain this codebase"
```

## Configuration

Config file: `~/.config/ion/config.toml`

```toml
openrouter_api_key = "sk-or-..."
model = "anthropic/claude-sonnet-4"
```

[MIT](LICENSE)
