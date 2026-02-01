# ion Status

## Current State

| Metric    | Value          | Updated    |
| --------- | -------------- | ---------- |
| Phase     | Stable         | 2026-01-31 |
| Status    | Review Done    | 2026-01-31 |
| Toolchain | stable         | 2026-01-22 |
| Tests     | 299 passing    | 2026-01-31 |
| Clippy    | pedantic clean | 2026-01-31 |

## Just Completed

**Code Review & Fixes** (2026-01-31):

- Deduplicated helper functions (cli.rs uses message_list's extract_key_arg)
- Removed unused Pipe trait
- Cleaned up test file structure (removed redundant mod tests wrappers)
- Added hook behavior documentation (last-wins semantics)
- Added tracing::warn for malformed JSON fallback

**Refactor Sprint** (2026-01-31):

- 12 commits: file splits, hook system, PickerNavigation trait
- Hook integration wired into ToolOrchestrator
- Added 79 automated tests (message_list, OAuth, CLI)

## Decision Needed

**OAuth Client IDs:** Using public client IDs from Codex CLI and Gemini CLI.

1. Keep borrowed IDs - works now, may break if upstream changes
2. Register our own - more stable, requires developer accounts

## Open Bugs

| ID      | Issue                    | Root Cause                  |
| ------- | ------------------------ | --------------------------- |
| tk-2bk7 | Resize clears scrollback | Needs preservation strategy |

## Top Priorities

1. OAuth testing with real subscriptions (tk-uqt6, tk-toyu)
2. Ctrl+R fuzzy history search (tk-g3dt)
3. Manual TUI testing if needed

## Key References

| Topic                 | Location                                |
| --------------------- | --------------------------------------- |
| Architecture overview | ai/DESIGN.md                            |
| OAuth design          | ai/design/oauth-subscriptions.md        |
| Module organization   | ai/design/module-structure.md           |
| Review report         | ai/review/refactor-sprint-2026-01-31.md |
