# Sprint 7 Review Summary

**Date:** 2026-01-25

## Overall Health

| Module    | Health | Critical | Important | Notes                     |
| --------- | ------ | -------- | --------- | ------------------------- |
| tui/      | GOOD   | 0        | 2         | Well-structured, minor UX |
| agent/    | GOOD   | 3        | 2         | Needs unwrap fixes        |
| provider/ | GOOD   | 2        | 2         | RwLock + timeout issues   |
| misc/     | GOOD   | 0        | 4         | MCP process lifecycle     |

## Critical Issues (Fix Immediately)

### Agent Module

1. **`execute_tools_parallel` unwrap panic** - `src/agent/mod.rs:710`
2. **Template unwraps** - `src/agent/context.rs:59, 150`
3. **Regex compiled per-call** - `src/agent/designer.rs:113`

### Provider Module

4. **RwLock poison not handled** - `src/provider/registry.rs` (6 locations)
5. **HTTP client no timeouts** - `src/provider/registry.rs:107`, `models_dev.rs:45`

## Important Issues (Fix Soon)

### TUI

1. Token percentage overflow risk - `src/tui/render.rs:368`
2. Slash command history index not reset - `src/tui/events.rs:196-258`

### Agent

3. Message queue poisoning ignored - `src/agent/mod.rs:285-294`
4. Compaction validation missing - `src/agent/mod.rs:345-362`

### Provider

5. Duplicated filter logic - `src/provider/registry.rs:348-459`
6. Ollama context window fallback too low (8192) - `src/provider/registry.rs:214`

### Misc

7. MCP process cleanup not guaranteed - `src/mcp/mod.rs:49-108`
8. Session store error context lost - `src/session/store.rs:49-51`
9. Config merge hides explicit defaults - `src/config/mod.rs:242-246`
10. Input history race condition - `src/session/store.rs:292-314`

## Performance

| Metric          | Value  | Status     |
| --------------- | ------ | ---------- |
| Startup time    | 4.3 ms | Excellent  |
| Binary size     | 32 MB  | Acceptable |
| Test time       | 0.28s  | Excellent  |
| Memory patterns | Clean  | No leaks   |

**Quick win:** Add `strip = true` to `[profile.release]` (saves 3 MB)

## Testing Gaps

- MCP module has no tests
- No panic tests for tool execution
- No mutex/RwLock poisoning tests
- No compaction edge case tests

## Refactor Recommendations

None required. All modules are well-structured. Issues are localized fixes.

## Action Plan

**Priority 1 (Critical):**

1. Fix unwrap in execute_tools_parallel
2. Fix RwLock poison handling in registry
3. Add HTTP client timeouts

**Priority 2 (Important):** 4. Fix template unwraps (use expect with message) 5. Make regex static in designer.rs 6. Handle message queue poisoning 7. Extract filter logic in registry

**Priority 3 (Nice to have):** 8. Add MCP process cleanup 9. Fix Ollama context fallback 10. Add strip=true to release profile
