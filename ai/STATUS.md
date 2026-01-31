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

**Code quality fixes** (2026-01-30):

- Split render.rs: 1086 → 692 lines (-36%)
- Improved 7 tool descriptions to prevent hallucinations
- Fixed review issues: underflow guards, is_dir caching
- FileCandidate struct caches is_dir (no syscalls in render loop)
- Deduplicated MAX_VISIBLE_ITEMS constant

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

| Module    | Files | Lines | Health | Notes                        |
| --------- | ----- | ----- | ------ | ---------------------------- |
| provider/ | 18    | ~2500 | GOOD   | Native HTTP, 3 backends      |
| tui/      | 25    | ~8500 | GOOD   | render.rs split, cleaner now |
| agent/    | 9     | ~900  | GOOD   | Decomposed, clean structure  |
| tool/     | 15    | ~2500 | GOOD   | Improved descriptions        |
| auth/     | 5     | ~800  | NEW    | OAuth complete               |
| session/  | 3     | ~600  | GOOD   | SQLite + WAL                 |
| skill/    | 3     | ~400  | GOOD   | YAML frontmatter             |
| mcp/      | 2     | ~300  | OK     | Needs tests                  |

## Top Priorities

1. Test TUI changes manually (autocomplete, history nav, images)
2. OAuth testing with real subscriptions
3. Consider Ctrl+R fuzzy history search (tk-g3dt)

## Key References

| Topic                 | Location                         |
| --------------------- | -------------------------------- |
| Architecture overview | ai/DESIGN.md                     |
| OAuth design          | ai/design/oauth-subscriptions.md |
| Module organization   | ai/design/module-structure.md    |
