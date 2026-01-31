# ion Status

## Current State

| Metric    | Value         | Updated    |
| --------- | ------------- | ---------- |
| Phase     | Provider Done | 2026-01-30 |
| Status    | Ready to Test | 2026-01-30 |
| Toolchain | stable        | 2026-01-22 |
| Tests     | 208 passing   | 2026-01-30 |
| Clippy    | clean         | 2026-01-30 |

## Just Completed

**Agent refactoring + UX improvements** (2026-01-30):

- Decomposed agent/mod.rs: 785 → 328 lines (events, retry, stream, tools modules)
- Pretty-print API error JSON: extracts message from raw JSON blobs
- StreamContext struct reduces params (9 → 5)
- Unified retry logic: retryable_category() replaces two functions
- Readline history: Ctrl+P/N for direct history navigation

## Current Focus

**TUI polish and testing** - limited automated testing for TUI, needs manual verification.

## Decision Needed

**OAuth Client IDs:** Using public client IDs from Codex CLI and Gemini CLI.

1. Keep borrowed IDs - works now, may break if upstream changes
2. Register our own - more stable, requires developer accounts

## Open Bugs

| ID      | Issue                    | Root Cause                  |
| ------- | ------------------------ | --------------------------- |
| tk-2bk7 | Resize clears scrollback | Needs preservation strategy |

## Module Health

| Module    | Files | Lines | Health | Notes                       |
| --------- | ----- | ----- | ------ | --------------------------- |
| provider/ | 18    | ~2500 | GOOD   | Native HTTP, 3 backends     |
| tui/      | 20    | ~6700 | OK     | render.rs needs split       |
| agent/    | 9     | ~900  | GOOD   | Decomposed, clean structure |
| tool/     | 15    | ~2500 | GOOD   | Orchestrator + spawn        |
| auth/     | 5     | ~800  | NEW    | OAuth complete              |
| session/  | 3     | ~600  | GOOD   | SQLite + WAL                |
| skill/    | 3     | ~400  | GOOD   | YAML frontmatter            |
| mcp/      | 2     | ~300  | OK     | Needs tests                 |

## Top Priorities

1. Test TUI changes manually (history nav, error display)
2. OAuth testing with real subscriptions
3. Split large TUI files (render.rs)

## Key References

| Topic                 | Location                         |
| --------------------- | -------------------------------- |
| Architecture overview | ai/DESIGN.md                     |
| OAuth design          | ai/design/oauth-subscriptions.md |
| Module organization   | ai/design/module-structure.md    |
