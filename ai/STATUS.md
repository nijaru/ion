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

**TUI Overhaul:**

- Progress line between chat and input: `⠼ Ionizing... (14s · ↑ 4.1k · ↓ 2.1k)`
- Token counting with tiktoken (input/output tracked separately)
- Message indicators: ↑ You, ↓ model-name, ⏺ tool
- Multiple queued messages (shown in chat with > prefix)
- Status line: `model · 56% (112k/200k) · [branch] · cwd`
- Spinner stuck bug fixed (is_running cleared on session complete)

**Previous Session:**

- Input always visible during agent execution
- Message queueing for mid-task steering
- CLI one-shot mode (`ion run`)

## Open Tasks

**Bugs:**

- [ ] tk-3jba: Ctrl+C not interruptible during tool execution

**UX:**

- [ ] tk-rre9: Up arrow recalls queued messages
- [ ] tk-otmx: Ctrl+G opens input in external editor
- [ ] tk-whde: Git diff stats in status line
- [ ] tk-arh6: Tool execution not visually obvious
- [ ] tk-o4uo: Modal escape handling

**Ideas:**

- [ ] tk-iegz: OpenRouter provider routing modal
- [ ] tui-textarea for better input editing
- [ ] tree-sitter for syntax highlighting

## Completed

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

- `ai/design/plugin-architecture.md` - Hook system, plugin format
- `ai/DECISIONS.md` - All architecture decisions
