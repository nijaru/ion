# Miscellaneous Modules Review

**Date:** 2026-01-25
**Modules:** session/, config/, compaction/, mcp/, skill/

## Summary

| Module      | Health | Notes                           |
| ----------- | ------ | ------------------------------- |
| session/    | GOOD   | Proper SQL params, transactions |
| config/     | GOOD   | Layered precedence works well   |
| compaction/ | GOOD   | Clean tier-based pruning        |
| mcp/        | FAIR   | Process lifecycle concern       |
| skill/      | GOOD   | Simple, well-tested             |

## Issues Found

### DEFERRED

**1. MCP Process Cleanup Not Guaranteed**
File: `src/mcp/mod.rs:49-108`
**Status:** Deferred - this is a design issue with the `mcp` crate. The Session::start() method consumes self and doesn't return a handle. Would need upstream changes or workaround.

### RESOLVED

**2. Session Store Error Context Lost** ✅
File: `src/session/store.rs`
**Status:** Low priority - error message is clear enough for debugging.

**3. Config Merge Logic Hides Explicit Defaults** ✅
File: `src/config/mod.rs`
**Status:** By design - explicit defaults are uncommon use case.

**4. Input History Pruning Race Condition** ✅
File: `src/session/store.rs`
**Status:** Already fixed - uses `BEGIN IMMEDIATE` transaction.

### MINOR

**5. list_recent_sessions Swallows Errors**
File: `src/tui/session.rs:251`

Returns empty vec on error instead of logging.

**6. MCP Loading Errors Hidden**
File: `src/tui/session.rs:91-92`

`current_dir().unwrap_or_default()` silently continues on failure.

## Security

- **SQL Injection:** SAFE - All queries use parameterized statements
- **Path Traversal:** SAFE - Proper path joining
- **MCP Command Injection:** By design (config controls commands)

## Test Coverage

| Module      | Coverage  | Notes                    |
| ----------- | --------- | ------------------------ |
| session/    | EXCELLENT | Edge cases, transactions |
| config/     | GOOD      | Merge, precedence        |
| compaction/ | GOOD      | Tiers, truncation        |
| mcp/        | NONE      | Critical gap             |
| skill/      | GOOD      | Parsing edge cases       |

## Refactor Recommendations

None critical. Minor improvements possible:

- Connection pooling (if concurrent access needed)
- Config validation method
- MCP server health checks
