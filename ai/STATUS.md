# ion Status

## Current State

| Metric    | Value          | Updated    |
| --------- | -------------- | ---------- |
| Phase     | Refactor Done  | 2026-01-31 |
| Status    | Ready to Test  | 2026-01-31 |
| Toolchain | stable         | 2026-01-22 |
| Tests     | 225 passing    | 2026-01-31 |
| Clippy    | pedantic clean | 2026-01-31 |

## Just Completed

**Code Review & Refactor Sprint** (2026-01-31):

- Phase 1: Performance + idiom improvements (CTE query, single-pass take_tail, format! → Print)
- Phase 2: Split 3 large files (composer 1103→4, highlight 841→5, session 740→6)
- Phase 3: PickerNavigation trait (18 match arms → 6 dispatch calls)
- Phase 4: Hook system + tool metadata types (foundations for extensibility)

## Current Focus

**TUI polish and testing** - features implemented, need manual verification.

## Decision Needed

**OAuth Client IDs:** Using public client IDs from Codex CLI and Gemini CLI.

1. Keep borrowed IDs - works now, may break if upstream changes
2. Register our own - more stable, requires developer accounts

## Open Bugs

| ID      | Issue                    | Root Cause                  |
| ------- | ------------------------ | --------------------------- |
| tk-2bk7 | Resize clears scrollback | Needs preservation strategy |

## Module Health

| Module    | Files | Lines | Health | Notes                            |
| --------- | ----- | ----- | ------ | -------------------------------- |
| provider/ | 18    | ~2500 | GOOD   | Native HTTP, 3 backends          |
| tui/      | 31    | ~7500 | GOOD   | Split composer/highlight/session |
| agent/    | 9     | ~900  | GOOD   | Decomposed, clean structure      |
| tool/     | 15    | ~2600 | GOOD   | Added ToolMetadata types         |
| hook/     | 1     | ~250  | NEW    | Hook system for extensibility    |
| auth/     | 5     | ~800  | GOOD   | OAuth complete                   |
| session/  | 3     | ~600  | GOOD   | SQLite + WAL                     |
| skill/    | 3     | ~400  | GOOD   | YAML frontmatter                 |
| mcp/      | 2     | ~300  | OK     | Needs tests                      |

## Top Priorities

1. Test TUI changes manually (autocomplete, history nav, images)
2. OAuth testing with real subscriptions
3. Consider Ctrl+R fuzzy history search (tk-g3dt)

## Remaining Refactor Work (optional)

- File splits: openai_compat/client.rs, registry.rs, events.rs, render.rs
- Completer logic deduplication
- Dynamic tool loading integration

## Key References

| Topic                 | Location                         |
| --------------------- | -------------------------------- |
| Architecture overview | ai/DESIGN.md                     |
| OAuth design          | ai/design/oauth-subscriptions.md |
| Module organization   | ai/design/module-structure.md    |
