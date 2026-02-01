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

**Refactor Sprint Complete** (2026-01-31):

- Phase 1: Performance + idiom improvements (CTE query, single-pass take_tail, format! → Print)
- Phase 2: Split 6 large files into focused modules:
  - `composer/` (1103→4 files), `highlight/` (841→5), `session/` (740→6)
  - `openai_compat/` (792→4 files), `registry/` (745→5), `render/` (695→5)
- Phase 3: PickerNavigation trait (18 match arms → 6 dispatch calls)
- Phase 4: Hook system + tool metadata types (foundations for extensibility)
- Phase 5: Hook integration (HookRegistry wired into ToolOrchestrator)

**Review completed** - All changes pass build, tests, and clippy pedantic.

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
