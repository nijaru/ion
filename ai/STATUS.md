# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | TUI v2 Complete | 2026-01-27 |
| Status     | Testing         | 2026-01-27 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 122 passing     | 2026-01-29 |
| Clippy     | 97 pedantic     | 2026-01-29 |
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

- Separator line not cleared on first message (tk-y0gs)
- Cursor off by 1 with pasted text / placeholder (tk-6gxy)
- Kimi k2.5 'Invalid request' errors on OpenRouter (tk-axae) - **FIX**: native provider using Anthropic Messages API (like pi)
- Progress line duplicates when switching terminal tabs during streaming (tk-7aem)
- Resize reflow clears pre-ion scrollback; decide preservation strategy (tk-2bk7)
- Exiting TUI leaves blank lines before shell prompt (tk-3o0l)
- --continue resume behavior needs verification (tk-7bcv)

## Recent Session

**2026-01-27:** Consolidated ai/research/ from 44 → 33 files. Key consolidations:

- `input-research.md` ← input/fuzzy files
- `context-management.md` ← context/compaction files
- `agent-survey.md` ← terminal-agents comparison

**2026-01-28:** TUI v2 review. Fixed cursor wrap drift + scrollback CRLF; added input normalization + markdown list cleanup; added input scroll; resize now reflows chat by clearing scrollback when chat exists.
**2026-01-28:** Anchored startup UI near header, added width-aware chat wrapping, and tightened markdown list rendering. Need terminal verification.
**2026-01-28:** Exit avoids adding system "Session closed" message; empty/system-only sessions are skipped (no prune on startup); list_recent filters to sessions with user messages; markdown renderer inserts paragraph/heading/list spacing and trims leading/trailing entry blanks; consecutive blank lines collapsed and blank tool lines skipped.
**2026-01-28:** Fixed error duplication, tool name sanitization (live + session load), session ID on exit, removed code block 2-space indent, added table rendering (full-width with box drawing + narrow fallback with "Header: Value" format).
**2026-01-28:** Investigated Kimi k2.5 errors - llm-connector ChatRequest lacks `extra`/`provider` field for OpenRouter routing. Known Kimi tool calling bugs. ProviderPrefs built but can't be sent to API without llm-connector changes.
**2026-01-29:** Sprint 12 clippy pedantic refactoring (139→97 warnings). Split run_tui/run_inner into focused helpers. Converted unused self to associated functions. Added #[allow] for intentional patterns. Session persistence on error fix. Architecture analysis vs Claude Code hooks system - gaps documented for future extensibility work.

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
