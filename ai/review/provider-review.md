# Provider Module Review

**Date:** 2026-01-25
**Status:** Good with critical fixes needed

## Summary

The provider module is well-architected with clean abstractions. **2 critical** and **2 important** issues found, plus refactor opportunities.

## Issues Found

### CRITICAL

**1. RwLock Poison State Not Handled**
File: `src/provider/registry.rs:117, 271, 344, 400, 516, 522`

All RwLock access uses `.unwrap()`:

```rust
let cache = self.cache.read().unwrap();
let mut cache = self.cache.write().unwrap();
```

If any thread panics while holding write lock, all subsequent ops panic.

Fix:

```rust
let cache = self.cache.read()
    .map_err(|_| anyhow!("Model cache lock poisoned"))?;
// OR
let cache = self.cache.read().unwrap_or_else(|e| e.into_inner());
```

**2. HTTP Client Created Without Timeouts**
Files: `src/provider/registry.rs:107`, `src/provider/models_dev.rs:45`

```rust
reqwest::Client::new()  // Default infinite timeout
```

Network calls can hang indefinitely.

Fix:

```rust
reqwest::Client::builder()
    .timeout(Duration::from_secs(30))
    .connect_timeout(Duration::from_secs(10))
    .build()?
```

### IMPORTANT

**3. Duplicated Filter Logic**
File: `src/provider/registry.rs:348-459`

`list_models()` and `list_models_from_vec()` have 55 lines of nearly identical filter logic. Worse: `list_models()` checks `prefs.ignore`/`prefs.only` but `list_models_from_vec()` doesn't.

Fix: Extract `apply_filter(m: &ModelInfo, filter: &ModelFilter, prefs: &ProviderPrefs) -> bool`

**4. Ollama Fallback Context Window Too Low**
File: `src/provider/registry.rs:214-216`

```rust
.unwrap_or(8192) // Default for older models
```

Modern Ollama models have 32k-128k context. 8192 artificially limits them.

Fix: Use 32768 as conservative default.

## Refactor Recommendations

1. **HTTP Client Configuration**: Create shared `create_http_client()` with consistent timeouts
2. **Error Type**: `Error::RateLimited` defined but never used - either implement 429 handling or remove
3. **Cache Cloning**: `get_models()` clones entire vec on every call - return `Arc<Vec<ModelInfo>>` instead

## Good Patterns

- Proper `anyhow::Context` for error messages
- `thiserror::Error` for structured errors
- No `pub use` re-exports
- `crate::` imports over `super::`
- Comprehensive test coverage
