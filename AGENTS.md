# ion

Fast, lightweight, open-source coding agent.

## Project Structure

| Directory | Purpose            |
| --------- | ------------------ |
| src/      | Rust source        |
| ai/       | AI session context |
| .tasks/   | Task tracking (tk) |

## Session Workflow

**Start:**

1. Read `ai/STATUS.md` for current state and open tasks
2. Run `tk ls` to see task list

**End:**

- Update `ai/STATUS.md`
- `tk done <id>` for completed work
- Commit changes

## Architecture

| Module    | Purpose                             |
| --------- | ----------------------------------- |
| provider/ | Multi-provider LLM via `llm` crate  |
| tool/     | Built-in tools + MCP client         |
| skill/    | SKILL.md loader                     |
| agent/    | Multi-turn loop, session management |
| tui/      | ratatui + crossterm chat interface  |

**Built-in tools:** read, write, edit, bash, glob, grep

**Providers:** Anthropic, Google, Groq, Ollama, OpenAI, OpenRouter (alphabetical, no default)

## Tech Stack

| Component | Choice            |
| --------- | ----------------- |
| TUI       | ratatui/crossterm |
| Async     | tokio             |
| HTTP      | reqwest           |
| Database  | rusqlite          |
| Tokens    | bpe-openai        |

## Code Standards

| Aspect    | Standard                      |
| --------- | ----------------------------- |
| Toolchain | stable                        |
| Edition   | Rust 2024                     |
| Errors    | anyhow (app), thiserror (lib) |
| Async     | tokio (network), sync (files) |

**Patterns:**

- `&str` over `String`, `&[T]` over `Vec<T>` where possible
- `crate::` over `super::`
- No `pub use` re-exports unless for downstream API
- Use `cargo add` for dependencies, never edit Cargo.toml versions directly

## Commands

```bash
cargo build              # Debug
cargo build --release    # Release
cargo test               # Test
cargo clippy             # Lint
cargo fmt                # Format
tk ls                    # List tasks
tk add "title"           # Add task
tk done <id>             # Complete task
```

## Rules

- **No AI attribution** in commits/PRs
- **Ask before**: PRs, publishing, force ops
- **Commit frequently**, push regularly
- **Never force push** to main/master
- **Task tracking**: Use `tk` for all work
- **Issue tracking**: When user reports bugs/issues, **immediately create a `tk` task**

## Reference

| File              | Purpose                |
| ----------------- | ---------------------- |
| ai/STATUS.md      | Current state, tasks   |
| ai/DECISIONS.md   | Architecture decisions |
| ai/design/\*.md   | Design documents       |
| ai/research/\*.md | Research notes         |

**Competitive reference:** Claude Code, Gemini CLI, opencode, pi-mono, amp
