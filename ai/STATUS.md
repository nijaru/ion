# ion Status

## Current State

| Metric    | Value               | Updated    |
| --------- | ------------------- | ---------- |
| Phase     | Core hardening      | 2026-02-06 |
| Status    | Prioritized         | 2026-02-06 |
| Toolchain | stable              | 2026-01-22 |
| Tests     | 325 passing         | 2026-02-06 |
| Clippy    | clean               | 2026-02-06 |
| TUI Lines | ~9,500 (excl tests) | 2026-02-04 |

## Current Focus

Full codebase audit completed 2026-02-06. Tool pass done, priorities reorganized around core functionality.

**What works well:** Agent loop, tool system (10 tools + MCP), TUI rendering, provider abstraction (7 providers), session persistence, skills, compaction.

**Key gaps:** Permission persistence, manual compaction, Google provider broken, streaming robustness, no web search tool.

## Priority Queue

### P2 — Core functionality gaps

| Task    | Title                                     | Why                                               |
| ------- | ----------------------------------------- | ------------------------------------------------- |
| tk-w1ou | Persist permanent tool approvals          | TODO in code; approvals lost on restart           |
| tk-ubad | /compact slash command                    | No way to manually trigger compaction             |
| tk-yy1q | Fix Google provider (Generative Lang API) | Broken provider — streaming+tools doesn't work    |
| tk-g1fy | Modular streaming interface               | Core agent UX; needed for Google fix + robustness |

### P3 — Important improvements

| Task    | Title                               | Why                                              |
| ------- | ----------------------------------- | ------------------------------------------------ |
| tk-75jw | Web search tool                     | Agents need web access; DuckDuckGo scraping      |
| tk-kqie | Streaming timeout / stale detection | Hung streams freeze the UI                       |
| tk-c1ij | Rate limit Retry-After parsing      | Better retry behavior with provider rate limits  |
| tk-4fyx | Compaction threshold tuning         | 55%/45% is aggressive, compacts every 3-4 turns  |
| tk-g8xo | Session cleanup / retention         | Old sessions accumulate, no auto-cleanup         |
| tk-2bk7 | Scrollback preservation on resize   | Content lost when terminal resizes               |
| tk-jqe6 | Group parallel tool calls in TUI    | Visual clutter from many simultaneous tool calls |
| tk-5h0j | Permission system audit             | Review for correctness and completeness          |

### P4 — Deferred

| Task    | Title                                 |
| ------- | ------------------------------------- |
| tk-ltyy | ask_user tool                         |
| tk-epd1 | TUI refactor: extract event handlers  |
| tk-5j06 | Semantic memory system                |
| tk-a2s8 | Extensible OAuth providers            |
| tk-o0g7 | Extensible API providers              |
| tk-ije3 | Hooks/plugin system architecture      |
| tk-ur3b | PDF handling                          |
| tk-9zri | Auto-backticks around pastes          |
| tk-4gm9 | Settings selector UI                  |
| tk-tnzs | Provider/model identity normalization |
| tk-imza | ast-grep integration                  |
| tk-8qwn | System prompt comparison research     |
| tk-iegz | OpenRouter routing modal              |

## Architecture Assessment (2026-02-06)

| Area        | Score | Key Strength              | Key Gap                          |
| ----------- | ----- | ------------------------- | -------------------------------- |
| Agent loop  | 85%   | Multi-turn, retry, cancel | No branching, limited recovery   |
| Tool system | 80%   | 10 tools + MCP + perms    | Approval persistence, no web     |
| TUI         | 75%   | Direct crossterm, md      | No /compact, resize issues       |
| Providers   | 80%   | 7 providers, streaming    | Google broken, no fallback       |
| Session/DB  | 78%   | SQLite WAL, resume        | No cleanup, no branching         |
| Config      | 80%   | 3-tier TOML, MCP          | No validation, no hot-reload     |
| Skills      | 85%   | YAML + lazy load          | No parameters, no composition    |
| Compaction  | 72%   | Token-based pruning       | Aggressive thresholds, no manual |

**Only 1 TODO in codebase:** `src/tool/mod.rs:129` — persist approvals to config.

## Key References

| Topic                  | Location                                       |
| ---------------------- | ---------------------------------------------- |
| Architecture           | ai/DESIGN.md                                   |
| TUI design             | ai/design/tui-v2.md                            |
| Tool pass design       | ai/design/tool-pass.md                         |
| Agent design           | ai/design/agent.md                             |
| TUI analysis           | ai/review/tui-analysis-2026-02-04.md           |
| Working dir patterns   | ai/research/working-directory-patterns-2026.md |
| Claude Code comparison | ai/research/claude-code-architecture.md        |
