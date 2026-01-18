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

ion config: `~/.ion/config.toml`

```toml
[provider]
default = "openrouter"
model = "anthropic/claude-sonnet-4"

[provider.openrouter]
api_key_env = "OPENROUTER_API_KEY"
```

Project config: `.ion/config.toml` (committed), `.ion/config.local.toml` (gitignored)

## Agent Instructions

ion reads `AGENTS.md` (or `CLAUDE.md` as fallback) from project root and user home:

| Location              | Purpose                 |
| --------------------- | ----------------------- |
| `./AGENTS.md`         | Project instructions    |
| `~/.agents/AGENTS.md` | User global (preferred) |
| `~/.ion/AGENTS.md`    | User global (fallback)  |

ion proposes `~/.agents/` as a universal location for AI agent files (instructions, skills, subagents) that works across tools.

## License

[MIT](LICENSE)
