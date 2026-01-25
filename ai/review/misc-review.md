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

### IMPORTANT

**1. MCP Process Cleanup Not Guaranteed**
File: `src/mcp/mod.rs:49-108`

`McpClient::spawn()` creates a child process but doesn't store a handle to manage its lifecycle. When `McpClient` is dropped, the MCP server process may become orphaned.

Fix: Store `Child` handle and implement `Drop` to kill process.

**2. Session Store Error Context Lost**
File: `src/session/store.rs:49-51`

```rust
std::fs::create_dir_all(parent).map_err(|_| {
    SessionStoreError::Database(rusqlite::Error::InvalidPath(parent.to_path_buf()))
})?;
```

Discards actual IO error, making debugging harder.

Fix: Preserve original error in message.

**3. Config Merge Logic Hides Explicit Defaults**
File: `src/config/mod.rs:242-246`

Merge compares against default values, preventing users from explicitly setting values to defaults to override lower-priority configs.

Fix: Use `Option<T>` for overridable values.

**4. Input History Pruning Race Condition**
File: `src/session/store.rs:292-314`

INSERT and DELETE are separate statements without a transaction.

Fix: Wrap in explicit transaction.

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
