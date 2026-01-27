# ion Status

## Current State

| Metric     | Value           | Updated    |
| ---------- | --------------- | ---------- |
| Phase      | 5 - Polish & UX | 2026-01-26 |
| Status     | Runnable        | 2026-01-26 |
| Toolchain  | stable          | 2026-01-22 |
| Tests      | 103 passing     | 2026-01-26 |
| Clippy     | 0 warnings      | 2026-01-26 |
| Visibility | **PUBLIC**      | 2026-01-22 |

## Flow Audit Results

All 6 P1 audit tasks complete. Findings:

| Flow               | Status | Bugs Fixed                         | Notes                                          |
| ------------------ | ------ | ---------------------------------- | ---------------------------------------------- |
| Input → Response   | ✅     | Dead ThinkingDelta handler removed | Flow is sound, no critical bugs                |
| Tool execution     | ✅     | None                               | Parallel exec + sequential approval works      |
| Session management | ✅     | /clear now starts new session      | Was leaving stale DB messages                  |
| Mode transitions   | ✅     | None                               | Approval interrupts any mode (by design)       |
| Cancel/interrupt   | ✅     | None                               | Ctrl+C silent when running is intentional      |
| Provider switching | ✅     | None                               | Edge cases cause clear errors, not silent bugs |

## Active: Sprint 10 - Stabilization & Refactor

Code review findings + refactoring. See ai/SPRINTS.md for full details.

| Task  | Description                 | Status  |
| ----- | --------------------------- | ------- |
| S10-1 | Extract formatting helpers  | PENDING |
| S10-2 | Split render_selector_shell | PENDING |
| S10-3 | Decompose stream_response   | PENDING |
| S10-4 | Agent review issues         | PENDING |
| S10-5 | Input/session issues        | PENDING |
| S10-6 | SQLite WAL mode             | PENDING |

### Review Issues Found

| Area        | Issue                                        | Severity |
| ----------- | -------------------------------------------- | -------- |
| Agent       | Queued messages don't update token display   | Low      |
| Agent       | JoinSet panic error unclear                  | Low      |
| Input       | Blob placeholder collision                   | Low      |
| Input       | History loses blobs on reload                | Low      |
| Session     | Model registry only recreated for OpenRouter | Low      |
| Session     | Load session loses tool details              | Low      |
| Persistence | No WAL mode                                  | Low      |

## Priority 2: Feature Completeness (after Sprint 10)

- Image attachment (tk-80az)
- Autocomplete (tk-ik05, tk-hk6p)

## Priority 3: Cost Optimization (Release)

- Anthropic caching (tk-268g) - 50-100x savings
- Destructive command guard (tk-qy6g)

## Architecture

| Module    | Health | Notes                           |
| --------- | ------ | ------------------------------- |
| tui/      | GOOD   | Well-structured, 6 submodules   |
| agent/    | GOOD   | Clean turn loop, subagent added |
| provider/ | GOOD   | Multi-provider abstraction      |
| tool/     | GOOD   | Orchestrator + spawn_subagent   |
| session/  | GOOD   | SQLite persistence              |
| skill/    | GOOD   | YAML frontmatter, lazy loading  |
| mcp/      | OK     | Needs tests, cleanup deferred   |

## Recent Completions

**TUI Polish & Formatting (2026-01-26)**

- Fixed code indentation stripped by `Wrap { trim: true }` → changed to `trim: false`
- Added `sanitize_for_display()` for robust text handling (tabs→4 spaces, strip \r, control chars)
- Trim message start/end while preserving internal formatting
- Retry messages: dim yellow in progress line (not inline chat)
- Code blocks: blank line after for visual separation
- Fixed retry_status not cleared on Finished/Error events
- 103 tests passing, reviewed via 3 parallel subagents

**TUI Rendering Fixes (2026-01-26)**

- Fixed tool output not showing during agent run (only skip last Agent entry)
- Added visual gap between chat history and progress line (Option B)
- Queued messages now show on dedicated line above spinner: " ↳ N messages queued"
- Empty line for gap when no messages queued

**Codebase Review & Refactor (2026-01-26)**

- Reviewed all modules (tui, agent, provider, session, mcp, skill)
- Found 13 issues, most already fixed in prior work
- Fixed: Provider filter duplication + bug (missing ignore/only in list_models_from_vec)
- Extracted: create_http_client() helper, model_matches_filter() function
- Deferred: MCP process cleanup (mcp crate design issue)

**Flow Audit Sprint (2026-01-26)**

- Audited all 6 core flows
- Fixed /clear to properly start new session
- Removed dead ThinkingDelta handler code

**Sprint 9 (2026-01-26)**

- Subagents: spawn_subagent tool, registry from ~/.agents/subagents/
- Thinking display: "thinking" → "thought for Xs", content hidden from chat
- Web fetch, YAML frontmatter, progressive skill loading

## Config

```
~/.agents/           # AGENTS.md, skills/, subagents/
~/.ion/              # config.toml, sessions.db, cache/
```
