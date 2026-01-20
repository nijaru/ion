# ion Architecture

**Date**: 2026-01-16
**Status**: Implementation
**Purpose**: Architecture for ion - local-first TUI coding agent

## Overview

Single-binary Rust TUI coding agent with:

- **All providers supported** - user configures model/provider (no hardcoding)
- **Native OmenDB memory** (only agent with persistent memory)
- **Skills system** (SKILL.md, Claude Code compatible) - behavior modification in main context
- **Sub-agents** (explorer, researcher, reviewer) - context isolation for expansion tasks
- Built-in tools + MCP client for ecosystem
- Budget-aware context assembly with auto-compaction

## Core Architecture Principles

### Skills vs Sub-Agents

**Skills** modify behavior in the main context (same conversation history):

- `developer` - code implementation
- `designer` - architecture planning
- `refactor` - code restructuring

**Sub-agents** isolate context for expansion tasks:

- `explorer` (Fast model) - find files, search patterns
- `researcher` (Full model) - web search, doc synthesis
- `reviewer` (Full model) - build, test, analyze

**Decision criteria**: Sub-agents are for context isolation (large input → small output), not behavior specialization. See `ai/design/sub-agents.md`.

### Model Selection

Binary choice for simplicity:

| Model    | Use Case                            |
| -------- | ----------------------------------- |
| **Fast** | Explorer sub-agent (Haiku-class)    |
| **Full** | Everything else (inherit from main) |

### Memory System

Budget-aware context assembly (unique differentiator):

```
Turn N:
  1. Query memory for similar past tasks/failures
  2. Assemble context with token budget
  3. Include relevant memories in prompt
  4. Agent executes
  5. Record outcome to memory
  6. If context > 80%, compact
```

## Configuration

### File Locations

```
~/.config/ion/
├── config.toml          # Global settings
├── models.toml          # Model definitions + favorites
└── keys.toml            # API keys (separate for security)

./.ion/
├── config.toml          # Project overrides

~/.local/share/ion/
├── sessions/            # Persisted sessions
└── memory/              # OmenDB + SQLite databases
```

**Override order**: Global → Project → CLI args → Env vars

### CLI Args

```bash
ion                           # Start TUI
ion "prompt"                  # Start TUI with first message
ion -c, --continue            # Continue last session
ion -r, --resume <id>         # Resume specific session
ion -m <model>                # Override model
```
