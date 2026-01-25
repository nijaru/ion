# Agent Module Review

**Date:** 2026-01-25
**Status:** Good with critical fixes needed

## Summary

The agent module is well-structured with good async patterns, proper cancellation handling, and thoughtful compaction logic. **3 critical** and **2 important** issues found.

## Issues Found

### CRITICAL

**1. Unwrap in execute_tools_parallel Can Panic**
File: `src/agent/mod.rs:710`

```rust
Ok(results.into_iter().map(|o| o.unwrap()).collect())
```

If tool results slots are `None` (cancelled tasks, early abort), this panics.

Fix:

```rust
Ok(results.into_iter()
    .collect::<Option<Vec<_>>>()
    .ok_or_else(|| anyhow!("Tool execution results incomplete"))?)
```

**2. Template Unwraps Can Panic**
File: `src/agent/context.rs:59, 150`

```rust
env.add_template("system", DEFAULT_SYSTEM_TEMPLATE).unwrap();
let template = self.env.get_template("system").unwrap();
```

If template syntax is ever broken, these panic.

Fix: Use `expect()` with message or propagate error.

**3. Regex Compiled on Every Call**
File: `src/agent/designer.rs:113`

```rust
let re = regex::Regex::new(r"(?s)\{.*\}").unwrap();
```

Compiled on every `plan()` call. Inefficient and can panic if regex invalid.

Fix: Use `once_cell::sync::Lazy` for static compilation.

### IMPORTANT

**4. Message Queue Lock Poisoning Not Handled**
File: `src/agent/mod.rs:285-294`

```rust
if let Ok(mut queue) = queue.lock() { ... }
```

Silently ignores poisoning, potentially dropping queued messages.

Fix: Handle poisoning explicitly with `poisoned.into_inner()`.

**5. Compaction Validation Missing**
File: `src/agent/mod.rs:345-362`

No validation that pruned messages are still valid (non-empty, alternating roles).

Fix: Add assertions after pruning.

## Good Patterns

- Excellent retry logic with exponential backoff
- Proper cancellation with `tokio::select!` and `CancellationToken`
- Good `JoinSet` usage for parallel tool execution
- Smart streaming fallback detection
- Comprehensive token tracking

## Refactor Recommendations

1. Extract retry logic to helper (duplicated between streaming/non-streaming)
2. Extract message queue handling to separate method
3. Add invariant checks to pruning

## Testing Gaps

- No panic tests for `execute_tools_parallel`
- No mutex poisoning tests
- No compaction edge case tests
