# Provider Module Review

**Date:** 2026-01-25
**Status:** Good with critical fixes needed

## Summary

The provider module is well-architected with clean abstractions. **2 critical** and **2 important** issues found, plus refactor opportunities.

## Issues Found

### RESOLVED

**1. RwLock Poison State Not Handled** ✅
File: `src/provider/registry.rs`
**Status:** Already fixed - all locations use `unwrap_or_else(|e| e.into_inner())`

**2. HTTP Client Created Without Timeouts** ✅
Files: `src/provider/registry.rs`, `src/provider/models_dev.rs`
**Status:** Already had timeouts (30s/10s). Refactored to use shared `create_http_client()` helper.

**3. Duplicated Filter Logic** ✅
File: `src/provider/registry.rs`
**Status:** Fixed - extracted `model_matches_filter()` function. Also fixed bug where `list_models_from_vec()` was missing `prefs.ignore`/`prefs.only` checks.

**4. Ollama Fallback Context Window Too Low** ✅
File: `src/provider/registry.rs`
**Status:** Already fixed - uses 32768 as default.

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
