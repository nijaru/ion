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

| Module    | Purpose                              |
| --------- | ------------------------------------ |
| provider/ | Custom multi-provider LLM (streaming + tools) |
| tool/     | Built-in tools + MCP client          |
| skill/    | SKILL.md loader                      |
| agent/    | Multi-turn loop, session management  |
| tui/      | Direct crossterm (no ratatui)        |

### TUI Architecture (v2)

- **Chat history**: Printed to stdout via `insert_before`, terminal handles scrollback natively
- **Bottom UI**: Direct cursor positioning at terminal height - ui_height
- **Input composer**: Custom with ropey-backed buffer + blob storage for large pastes
- **Rendering**: Direct crossterm escape sequences, pulldown-cmark for markdown

**Built-in tools:** read, write, edit, bash, glob, grep

**Providers:** Anthropic, Google, Groq, Kimi, Ollama, OpenAI, OpenRouter

## Tech Stack

| Component | Choice         |
| --------- | -------------- |
| TUI       | crossterm      |
| Markdown  | pulldown-cmark |
| Async     | tokio          |
| HTTP      | reqwest        |
| Database  | rusqlite       |
| Tokens    | bpe-openai     |

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

## Bug Investigation

**Before investigating:**

1. `tk show <id>` - check existing notes in task log
2. Search ai/ for prior research: `grep -ri "keyword" ai/`
3. Check git log for related commits

**During investigation:**

- Log findings immediately: `tk log <id> "finding"`
- Include: error messages, root cause hypothesis, files involved
- For complex bugs, note which ai/ file has detailed analysis

**After investigating:**

- Update task with conclusion (fixed, needs more info, blocked by X)
- If blocked by external dependency, note the specific limitation
- Cross-reference related tasks if issues are distinct but confused

**Never claim something is undocumented without searching first.**

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
- `ai/design/permissions-v2.md` - Read/Write modes, sandbox

**Key research:**

- `ai/research/agent-survey.md` - Competitive analysis (6 agents)
- `ai/research/tool-display-patterns-2026.md` - Tool output UX
- `ai/research/edit-tool-patterns-2026.md` - Edit tool design

**Competitive reference:** Claude Code, Gemini CLI, opencode, pi-mono, amp
