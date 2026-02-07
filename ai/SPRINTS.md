# Sprint Plan: ion

## Status

| Sprint | Goal                | Status   |
| ------ | ------------------- | -------- |
| 0-14   | Foundation + TUI v2 | COMPLETE |
| 15     | Code Quality + UX   | ACTIVE   |

## Sprint 15: Code Quality + UX

**Goal:** Fix critical code issues from audit, improve TUI UX gaps vs competitors.
**Scope:** Quick wins from code quality audit + TUI streaming text display.
**Reviews:** `ai/review/{architecture,tui-ux,code-quality}-review-2026-02-06.md`

### Phase 1: Code Quality Quick Wins

| #   | Task                                                | File(s)                              | Status |
| --- | --------------------------------------------------- | ------------------------------------ | ------ |
| 1   | Fix unwrap panic risk in gemini Bearer token        | `provider/subscription/gemini.rs:50` |        |
| 2   | Fix unwrap panic risk in google auth                | `auth/google.rs:307-320`             |        |
| 3   | Fix unwrap in skill parser (use if-let)             | `skill/mod.rs:112,333`               |        |
| 4   | Delete dead Explorer struct                         | `agent/explorer.rs`                  |        |
| 5   | Delete dead ToolSource/ToolCapability/ToolMetadata  | `tool/types.rs:166-211`              |        |
| 6   | Delete unused HookPoint::OnError, OnResponse        | `hook/mod.rs:17-19`                  |        |
| 7   | Replace `pub use types::*` with explicit re-exports | `tool/mod.rs`, `provider/mod.rs`     |        |
| 8   | Extract from_provider shared logic                  | `provider/client.rs:41-143`          |        |

### Phase 2: TUI UX Improvements

| #   | Task                                        | File(s)                     | Status |
| --- | ------------------------------------------- | --------------------------- | ------ |
| 9   | Extract slash command dispatch method       | `tui/events.rs:347-458`     |        |
| 10  | Increase tool result max lines 5→10         | `tui/message_list.rs`       |        |
| 11  | Show thinking indicator in progress line    | `tui/render/direct.rs`      |        |
| 12  | Streaming text display (incremental render) | `tui/render/`, `tui/run.rs` |        |

### Phase 3: Architecture (if time)

| #   | Task                                          | File(s)                                   | Status |
| --- | --------------------------------------------- | ----------------------------------------- | ------ |
| 13  | Consume provider-reported usage in agent loop | `agent/mod.rs`, `agent/stream.rs`         |        |
| 14  | Replace uuid_v4 with proper UUID              | `provider/subscription/gemini.rs:220-236` |        |

## Completed Sprints

| Sprint | Goal                     | Completed  |
| ------ | ------------------------ | ---------- |
| 0-10   | Foundation               | 2026-01    |
| 11     | TUI v2: Remove ratatui   | 2026-01-27 |
| 12     | Clippy Pedantic          | 2026-01-29 |
| 13     | Agent Loop Decomposition | 2026-01-31 |
| 14     | TUI Refactoring          | 2026-02-04 |

Details in `ai/sprints/archive-0-10.md` and git history.
