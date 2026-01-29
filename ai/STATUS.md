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

**Fixed 2026-01-29:**

- ~~Separator line not cleared on first message (tk-y0gs)~~
- ~~Cursor off by 1 with pasted text / placeholder (tk-6gxy)~~ - Root cause: control chars in placeholder had zero display width
- ~~Exiting TUI leaves blank lines before shell prompt (tk-3o0l)~~

**Open bugs:**

- Progress line duplicates when switching terminal tabs during streaming (tk-7aem)
- Resize reflow clears pre-ion scrollback; decide preservation strategy (tk-2bk7)
- --continue resume behavior needs verification (tk-7bcv)
- Session ID printed on startup even with no messages (should skip empty sessions)

## llm-connector Limitations

Three distinct issues blocked by llm-connector:

| Issue                       | Description                                            | Impact                                                     |
| --------------------------- | ------------------------------------------------------ | ---------------------------------------------------------- |
| OpenRouter `provider` field | ChatRequest lacks `provider`/`extra` field for routing | Can't specify preferred provider for multi-provider models |
| Anthropic `cache_control`   | No cache_control support in messages                   | Missing 50-100x cost savings on prompt caching             |
| Request customization       | No way to add provider-specific fields                 | Limits per-provider features                               |

**Options:**

1. Remove llm-connector, implement direct API calls (~400-500 LOC total)
2. Fork llm-connector and add missing fields
3. Submit upstream PRs and wait

**Note:** These are general llm-connector issues, not Kimi-specific.

## Kimi Provider Status

- Native Kimi provider added (api.moonshot.ai, OpenAI-compatible)
- Dynamic model fetching via /v1/models
- Kimi errors on OpenRouter (tk-axae) are SEPARATE from the provider routing issue above - root cause unclear, possibly OpenRouter-side

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
**2026-01-29:** Implemented dev plan phases 1-3: Fixed UI bugs (cursor off-by-1, separator clearing, exit blank lines, width-aware wrapping); Added markdown ordered lists, blockquotes, horizontal rules; Added filter input Ctrl-W/Ctrl-U shortcuts; Added native Kimi (Moonshot) provider with OpenAI-compatible API.
**2026-01-29:** Documentation cleanup. Separated three distinct llm-connector issues (OpenRouter routing, Anthropic caching, request customization). Cursor bug root cause was control chars in placeholder having zero display width - fixed with visible chars «Pasted #N». Found bug: session ID printed on startup even with no messages.

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
