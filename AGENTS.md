# ion - Local-First TUI Coding Agent

**Rust TUI coding agent with native OmenDB memory integration.**

## Vision

```
ion = Rust TUI Agent (ratatui)
     + Native Memory (OmenDB + rusqlite)
     + Skills System (SKILL.md)
     + Multi-Provider LLM (OpenRouter primary)
```

**Differentiation**: Budget-aware memory context assembly vs Goose (full injection) / Claude Code (no memory)

## Project Structure

| Directory | Purpose                 |
| --------- | ----------------------- |
| src/      | Rust source             |
| legacy/   | Archived TS/Python code |
| ai/       | **AI session context**  |

### AI Context

**Session files** (read every session):

- ai/STATUS.md - Current state (read FIRST)
- ai/DECISIONS.md - Architecture decisions
- ai/design/rust-architecture.md - Implementation guide

**Reference files**:

- ai/research/competitive/ - Agent analysis
- ai/research/memory-architectures-comparison.md - Memory system research

## Technology Stack

| Component | Choice              | Why                      |
| --------- | ------------------- | ------------------------ |
| Language  | Rust                | Single binary, fast      |
| TUI       | ratatui + crossterm | Mature, async-friendly   |
| Async     | tokio               | Standard, well-supported |
| HTTP      | reqwest             | Production-grade         |
| Database  | rusqlite            | Embedded, zero deps      |
| Vectors   | omendb              | Native Rust, HNSW+ACORN  |
| Tokens    | tiktoken-rs         | OpenAI-compatible        |

## Commands

```bash
cargo build              # Debug build
cargo build --release    # Release build
cargo test               # Run tests
cargo clippy             # Lint
cargo fmt                # Format
```

## Supported Models

**PRIMARY** (via OpenRouter):

- `deepseek/deepseek-v4` - Main model (when available)
- `anthropic/claude-sonnet-4` - Fallback

**DIRECT**:

- Anthropic API (Claude)
- Ollama (local models)

## Architecture

**Core Modules**:

- **memory/** - Native OmenDB + rusqlite, budget-aware context assembly
- **provider/** - Multi-provider LLM abstraction
- **tool/** - Built-in tools (read, write, edit, bash, glob, grep) + MCP client
- **skill/** - SKILL.md loader (Claude Code compatible)
- **agent/** - Multi-turn loop, session management
- **tui/** - ratatui chat interface

**Key Innovation**: Budget-aware memory context assembly

- Fills context up to token budget with highest-relevance memories
- ACE scoring (+17% accuracy) + RRF fusion (+6% accuracy)
- Query classification skips memory for transactional queries

## MVP Features

| Feature        | Status  | Notes                               |
| -------------- | ------- | ----------------------------------- |
| Provider       | Pending | OpenRouter primary                  |
| TUI            | Pending | ratatui chat interface              |
| Built-in tools | Pending | Read, Write, Edit, Bash, Glob, Grep |
| Agent loop     | Pending | Multi-turn until complete           |
| Memory         | Pending | Native OmenDB integration           |
| Skills         | Pending | SKILL.md loader                     |

## Workflow

**Session Start**:

1. Read ai/STATUS.md
2. Run `tk ready` for available work
3. Reference ai/design/rust-architecture.md

**Session End**:

- Update ai/STATUS.md
- `tk done <id>` completed work

## Code Standards

| Aspect     | Standard                                   |
| ---------- | ------------------------------------------ |
| Toolchain  | nightly (omendb requires portable_simd)    |
| Edition    | Rust 2024                                  |
| Errors     | anyhow (app), thiserror (lib)              |
| Async      | tokio (network), rayon (CPU), sync (files) |
| Formatting | rustfmt                                    |
| Linting    | clippy                                     |

## Rules

- **No AI attribution** in commits/PRs
- **Ask before**: PRs, publishing, force ops
- **Commit frequently**, push regularly
- **Never force push** to main/master
- **Task tracking**: Use `tk`
