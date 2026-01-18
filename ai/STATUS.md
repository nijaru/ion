# ion Status

## Current State

| Metric    | Value           | Updated    |
| --------- | --------------- | ---------- |
| Phase     | 5 - Polish & UX | 2026-01-18 |
| Focus     | TUI Polish      | 2026-01-18 |
| Status    | Runnable        | 2026-01-18 |
| Toolchain | stable          | 2026-01-18 |
| Tests     | 49 passing      | 2026-01-18 |

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Built-in providers (OpenRouter, Anthropic, Ollama)
- Built-in tools (read, write, edit, bash, glob, grep)
- MCP client
- Session management (rusqlite)
- Claude Code-compatible hook system

**Memory** (archived to nijaru/ion-archive):

- Will re-implement after TUI agent is fully working
- OmenDB + native Rust embeddings

## Session Accomplishments

**Memory Removal / Stable Switch:**

- Archived memory code to nijaru/ion-archive
- Removed src/memory/ module and all references
- Switched rust-toolchain.toml from nightly to stable
- Removed omendb dependency (required nightly for portable_simd)
- Updated all docs to reflect current state

**Dependency Upgrades (completed):**

- grep tool now uses `ignore` crate (respects .gitignore)
- glob tool now uses `globset` (via ignore)
- Replaced `tiktoken-rs` with `bpe-openai` (4x faster)
- Removed unused deps: walkdir, serde_yaml, glob

**Documentation:**

- Updated GitHub repo description
- Cleaned README (added WIP note, removed nightly requirement)
- Updated AGENTS.md/CLAUDE.md

## Open Tasks

**Bugs:**

- [ ] tk-3jba: Ctrl+C not interruptible during tool execution

**UX:**

- [ ] tk-otmx: Ctrl+G opens input in external editor
- [ ] tk-whde: Git diff stats in status line
- [ ] tk-arh6: Tool execution not visually obvious
- [ ] tk-o4uo: Modal escape handling

**Ideas:**

- [ ] tk-iegz: OpenRouter provider routing modal
- [ ] tk-smqs: Diff highlighting for edits (like Claude Code)
- [ ] tui-textarea for better input editing
- [ ] tree-sitter for syntax highlighting

## Completed

- [x] Dependency upgrades (grep, glob, tokenizer)
- [x] Memory removal / stable Rust switch
- [x] Config persistence (model selection saved)
- [x] Up arrow recalls queued messages
- [x] ANSI color support for tool output
- [x] Input always visible
- [x] Message queueing (multiple messages)
- [x] Progress line with elapsed + token counts
- [x] Message indicators (↑/↓/⏺)
- [x] Model name in messages (not "ion")
- [x] Status line with token counts
- [x] Spinner stuck fix
- [x] Approval dialog wording
- [x] CLI one-shot mode

## Design Documents

- `ai/design/tui.md` - TUI interface spec
- `ai/design/diff-highlighting.md` - Diff rendering plan
- `ai/design/interrupt-handling.md` - Ctrl+C behavior spec
- `ai/design/dependency-upgrades.md` - Lib replacements (done)
- `ai/design/plugin-architecture.md` - Hook system, plugin format
- `ai/DECISIONS.md` - All architecture decisions
