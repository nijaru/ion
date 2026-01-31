# ion Status

## Current State

| Metric    | Value         | Updated    |
| --------- | ------------- | ---------- |
| Phase     | Provider Done | 2026-01-30 |
| Status    | Ready to Test | 2026-01-30 |
| Toolchain | stable        | 2026-01-22 |
| Tests     | 223 passing   | 2026-01-30 |
| Clippy    | clean         | 2026-01-30 |

## Just Completed

**Input enhancements + review fixes** (2026-01-30):

- File autocomplete: `@path` with fuzzy matching, hidden files with `@.`
- Command autocomplete: `/` at start shows commands with descriptions
- Image attachment: `@image:path.png` or `@image:"path with spaces.png"`
- Review fixes: OOM prevention, backspace behavior, session resume

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

| Module    | Files | Lines | Health | Notes                       |
| --------- | ----- | ----- | ------ | --------------------------- |
| provider/ | 18    | ~2500 | GOOD   | Native HTTP, 3 backends     |
| tui/      | 24    | ~8300 | OK     | +autocomplete/images, big   |
| agent/    | 9     | ~900  | GOOD   | Decomposed, clean structure |
| tool/     | 15    | ~2500 | GOOD   | Orchestrator + spawn        |
| auth/     | 5     | ~800  | NEW    | OAuth complete              |
| session/  | 3     | ~600  | GOOD   | SQLite + WAL                |
| skill/    | 3     | ~400  | GOOD   | YAML frontmatter            |
| mcp/      | 2     | ~300  | OK     | Needs tests                 |

## Top Priorities

1. Test TUI changes manually (autocomplete, history nav)
2. OAuth testing with real subscriptions
3. Split large TUI files (render.rs)

## Key References

| Topic                 | Location                         |
| --------------------- | -------------------------------- |
| Architecture overview | ai/DESIGN.md                     |
| OAuth design          | ai/design/oauth-subscriptions.md |
| Module organization   | ai/design/module-structure.md    |
