# ion Status

## Current State

| Metric    | Value          | Updated    |
| --------- | -------------- | ---------- |
| Phase     | Testing        | 2026-01-31 |
| Status    | Tests Added    | 2026-01-31 |
| Toolchain | stable         | 2026-01-22 |
| Tests     | 304 passing    | 2026-01-31 |
| Clippy    | pedantic clean | 2026-01-31 |

## Just Completed

**Automated Test Coverage** (2026-01-31):

Added 79 new tests (225 → 304):

- `message_list.rs`: 43 tests (formatting, scrolling, tool output)
- `auth/openai.rs`: 5 tests (OAuth URL building, endpoints)
- `auth/google.rs`: 6 tests (OAuth URL, offline access, scopes)
- `cli.rs`: 25 tests (arg parsing, commands, permissions)

**Refactor Sprint Complete** (2026-01-31):

- Phases 1-5: Performance, file splits, PickerNavigation trait, hook system
- Hook integration wired into ToolOrchestrator

## Decision Needed

**OAuth Client IDs:** Using public client IDs from Codex CLI and Gemini CLI.

1. Keep borrowed IDs - works now, may break if upstream changes
2. Register our own - more stable, requires developer accounts

## Open Bugs

| ID      | Issue                    | Root Cause                  |
| ------- | ------------------------ | --------------------------- |
| tk-2bk7 | Resize clears scrollback | Needs preservation strategy |

## Module Health

| Module    | Files | Lines | Health | Notes                                   |
| --------- | ----- | ----- | ------ | --------------------------------------- |
| provider/ | 24    | ~2700 | GOOD   | Split openai_compat + registry          |
| tui/      | 36    | ~7500 | GOOD   | Split composer/highlight/session/render |
| agent/    | 9     | ~900  | GOOD   | Decomposed, clean structure             |
| tool/     | 15    | ~2700 | GOOD   | Hook integration complete               |
| hook/     | 1     | ~250  | READY  | Integrated into ToolOrchestrator        |
| auth/     | 5     | ~800  | GOOD   | OAuth complete                          |
| session/  | 3     | ~600  | GOOD   | SQLite + WAL                            |
| skill/    | 3     | ~400  | GOOD   | YAML frontmatter                        |
| mcp/      | 2     | ~300  | OK     | Needs tests                             |

## Top Priorities

1. Test TUI changes manually (autocomplete, history nav, images)
2. OAuth testing with real subscriptions
3. Consider Ctrl+R fuzzy history search (tk-g3dt)

## Deferred Items

- **Completer trait**: Analyzed, minimal duplication (~24 lines), different acceptance semantics. Not worth abstracting.
- **events.rs split**: Now 702 lines after PickerNavigation refactor, well-organized. No split needed.

## Key References

| Topic                 | Location                         |
| --------------------- | -------------------------------- |
| Architecture overview | ai/DESIGN.md                     |
| OAuth design          | ai/design/oauth-subscriptions.md |
| Module organization   | ai/design/module-structure.md    |
