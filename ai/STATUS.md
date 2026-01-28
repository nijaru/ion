# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | TUI v2 Complete | 2026-01-27 |
| Status     | Testing         | 2026-01-27 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 113 passing     | 2026-01-27 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## TUI Architecture

| Version | Approach                            | Status                     |
| ------- | ----------------------------------- | -------------------------- |
| v1      | Viewport::Inline(15) fixed height   | ABANDONED - gaps, cursor   |
| v2      | Direct crossterm, native scrollback | COMPLETE - ratatui removed |

## TUI v2 Architecture

**Core model:**

1. Chat → insert_before pattern (ScrollUp + print above UI)
2. Bottom UI → cursor positioning + clear/redraw at height-ui_height
3. Resize → clear screen, reprint all chat from `message_list`
4. Exit → clear bottom UI only, chat stays in scrollback

**Implemented:**

- `draw_direct()` - bottom UI rendering
- `render_progress_direct()`, `render_input_direct()`, `render_status_direct()`
- `render_selector_direct()` for picker modals
- `render_markdown()` using pulldown-cmark
- `parse_ansi_line()` for ANSI SGR parsing
- Progress line styling (cyan spinner, dim elapsed time)

**Dependencies removed:**

- ratatui, tui-markdown, ansi-to-tui

**Dependencies added:**

- pulldown-cmark

## Known Issues

- Kimi k2.5 API returns malformed JSON (tk-1lso)
- Input composer lacks scroll offset for long input (tk-28a4)
- Progress line duplicates when switching terminal tabs during streaming (tk-7aem)

## Recent Session

**2026-01-27:** Consolidated ai/research/ from 44 → 33 files. Key consolidations:

- `input-research.md` ← input/fuzzy files
- `context-management.md` ← context/compaction files
- `agent-survey.md` ← terminal-agents comparison

**2026-01-28:** TUI v2 review. Fixed cursor wrap drift + scrollback CRLF; added input normalization + markdown list cleanup; logged input scroll + tab-switch progress duplication.

## Module Health

| Module    | Health | Notes                     |
| --------- | ------ | ------------------------- |
| tui/      | GOOD   | v2 complete, testing      |
| agent/    | GOOD   | Clean turn loop           |
| provider/ | OK     | llm-connector limitations |
| tool/     | GOOD   | Orchestrator + spawn      |
| session/  | GOOD   | SQLite persistence + WAL  |
| skill/    | GOOD   | YAML frontmatter          |
| mcp/      | OK     | Needs tests               |

## Key Files

**Design:**

- `ai/design/tui-v2.md` - TUI architecture and implementation plan

**Research (consolidated):**

- `ai/research/agent-survey.md` - Agent comparison (Claude Code, Codex, Gemini, etc.)
- `ai/research/context-management.md` - Context compaction strategies
- `ai/research/input-research.md` - Input handling, fuzzy matching
- `ai/research/inline-tui-patterns-2026.md` - TUI patterns across ecosystems
