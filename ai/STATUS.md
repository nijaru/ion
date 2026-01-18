# ion Status

## Current State

| Metric | Value           | Updated    |
| ------ | --------------- | ---------- |
| Phase  | 5 - Polish & UX | 2026-01-18 |
| Focus  | TUI Polish      | 2026-01-18 |
| Status | Runnable        | 2026-01-18 |
| Tests  | 51 passing      | 2026-01-18 |

## Architecture

**Core Agent** (ion binary):

- TUI + Agent loop
- Built-in providers (OpenRouter, Anthropic, Ollama)
- Built-in tools (read, write, edit, bash, glob, grep)
- MCP client
- Session management
- Claude Code-compatible hook system

**Memory Plugin** (ion-memory, separate):

- OmenDB integration
- Loaded via hook system
- Can be skipped for minimal agent

## Session Accomplishments

**Dependency Audit:**

- Full audit of Cargo.toml for SOTA replacements
- Created `ai/design/dependency-upgrades.md` with research and plan
- Added 4 tasks for dependency work (tk-ykpu, tk-cfmz, tk-ha1x, tk-9tkf)

**Key Findings:**

- `ignore` crate already in deps but unused - grep/glob should use it
- `tiktoken-rs` can be replaced with GitHub's `bpe` (4x faster)
- `walkdir`, `serde_yaml`, `glob` can be removed (unused/deprecated/redundant)

## Open Tasks

**Dependencies (HIGH):**

- [ ] tk-ykpu: Upgrade grep tool to use ignore crate
- [ ] tk-cfmz: Upgrade glob tool to use globset
- [ ] tk-ha1x: Remove unused deps (walkdir, serde_yaml, glob)
- [ ] tk-9tkf: Replace tiktoken-rs with GitHub bpe crate

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

- [x] Config persistence (model selection saved)
- [x] Up arrow recalls queued messages (tk-rre9)
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

- `ai/design/tui.md` - TUI interface spec (keybindings, layout, features)
- `ai/design/diff-highlighting.md` - Diff rendering implementation plan
- `ai/design/interrupt-handling.md` - Ctrl+C behavior spec
- `ai/design/dependency-upgrades.md` - SOTA lib replacements (grep, glob, tokenizer)
- `ai/design/plugin-architecture.md` - Hook system, plugin format
- `ai/DECISIONS.md` - All architecture decisions
