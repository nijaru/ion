# ion

Fast, lightweight TUI coding agent.

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

### TUI Architecture

- **Chat history**: Printed to stdout via `insert_before`, terminal handles scrollback natively
- **Bottom UI**: We manage (progress, input, status) - stays at bottom via `Viewport::Inline`
- **Input composer**: Custom `ComposerWidget` with own cursor/wrap calculation
  - `ComposerBuffer` - ropey-backed text buffer with blob storage for large pastes
  - `ComposerState` - cursor position, scroll state, stash
  - **Known bug**: Cursor position off-by-one on wrapped lines (accumulates)
- **Rendering**: Uses ratatui's `Paragraph` with `Wrap` for display, but cursor calculation is custom

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

**ai/ directory contents:**

| File/Dir        | Purpose                                           |
| --------------- | ------------------------------------------------- |
| ai/STATUS.md    | Current state, open tasks, session notes          |
| ai/DECISIONS.md | Architecture decisions with context and rationale |
| ai/DESIGN.md    | High-level system design                          |
| ai/design/      | Detailed designs (TUI, config, permissions, etc)  |
| ai/research/    | Research notes (agents, patterns, crates)         |
| ai/ideas/       | Future feature ideas                              |

**Key design docs:**

- `ai/design/tui.md` - TUI interface spec
- `ai/design/config-system.md` - Config hierarchy
- `ai/design/permission-system.md` - CLI flags, modes, sandbox

**Key research:**

- `ai/research/tool-display-patterns-2026.md` - Tool output UX
- `ai/research/tui-agents-comparison-2026.md` - Competitive analysis
- `ai/research/edit-tool-patterns-2026.md` - Edit tool design

**Competitive reference:** Claude Code, Gemini CLI, opencode, pi-mono, amp
