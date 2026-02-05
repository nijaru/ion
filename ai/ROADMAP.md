# ion Roadmap

**Updated:** 2026-02-04

## Completed

- ✅ Provider layer (native HTTP, cache_control, provider routing)
- ✅ Anthropic caching support
- ✅ Kimi/DeepSeek reasoning extraction
- ✅ OAuth infrastructure (PKCE, callback server)
- ✅ Skills YAML frontmatter
- ✅ Subagent support
- ✅ TUI v2 (direct crossterm, no ratatui)
- ✅ TUI refactoring (Sprint 14 - panic fixes, code deduplication)

## Priority Backlog

### P2 - Architecture

| Task          | ID      | Description                               |
| ------------- | ------- | ----------------------------------------- |
| Memory system | tk-5j06 | Semantic memory for cross-session context |

### P3 - UX Improvements

| Task                  | ID      | Description                |
| --------------------- | ------- | -------------------------- |
| Ctrl+R history search | tk-g3dt | Fuzzy search input history |
| Settings UI           | tk-4gm9 | Settings selector modal    |
| Tool output format    | tk-6ydy | Review display patterns    |

### P3 - Provider/Config

| Task                 | ID      | Description                      |
| -------------------- | ------- | -------------------------------- |
| Extensible providers | tk-o0g7 | Config-defined API providers     |
| Google provider fix  | tk-yy1q | Standard Generative Language API |

### P3 - Tools

| Task                 | ID      | Description            |
| -------------------- | ------- | ---------------------- |
| ast-grep integration | tk-imza | Structural code search |
| Web search tool      | tk-1y3g | Search integration     |
| PDF handling         | tk-ur3b | pdf2text integration   |

## Deferred

| Task                    | Notes                                           |
| ----------------------- | ----------------------------------------------- |
| OAuth subscription      | ChatGPT/Gemini - unofficial APIs, deprioritized |
| Plugin system           | Waiting for core completion                     |
| Scrollback preservation | tk-2bk7 - complex terminal handling             |

See `tk ls` for full task list (20 items).
