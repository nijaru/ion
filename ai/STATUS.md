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

**Documentation:**

- Added descriptions to all 7 open tk tasks
- Created `ai/design/diff-highlighting.md` - full implementation spec
- Created `ai/design/interrupt-handling.md` - Ctrl+C behavior spec
- Updated `ai/design/tui.md` with current state and planned features

**Config Persistence:**

- Config::save() method to persist settings to disk
- Model selection persisted (no more repeated setup on each run)
- Renamed `default_model` to `model` in Config (cleaner naming)

**Message Queue Improvements:**

- Up arrow recalls queued messages back to input for editing
- Changed from channel-based to shared Mutex<Vec> queue
- Messages can be canceled before agent receives them

**ANSI Color Support:**

- Added ansi-to-tui crate for parsing ANSI escape sequences
- Tool output preserves colors (git diff, ls, etc.)
- Bash tool forces color output via CLICOLOR_FORCE, FORCE_COLOR env vars

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
- [ ] tui-textarea for better input editing
- [ ] tree-sitter for syntax highlighting
- [ ] Diff highlighting for edits (like Claude Code)

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
- `ai/design/plugin-architecture.md` - Hook system, plugin format
- `ai/DECISIONS.md` - All architecture decisions
