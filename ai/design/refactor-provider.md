# Provider Module Refactor

Three targeted fixes for robustness and maintainability in the provider module.

## Issue 1: RwLock Poison Handling

### Current State

All RwLock accesses already use `unwrap_or_else(|e| e.into_inner())` for poison recovery:

| Location     | Method           | Current Pattern                        |
| ------------ | ---------------- | -------------------------------------- |
| Line 127     | `cache_valid()`  | `unwrap_or_else(\|e\| e.into_inner())` |
| Line 281     | `fetch_models()` | `unwrap_or_else(\|e\| e.into_inner())` |
| Line 356-357 | `get_models()`   | `unwrap_or_else(\|e\| e.into_inner())` |
| Line 415     | `list_models()`  | `unwrap_or_else(\|e\| e.into_inner())` |
| Line 531     | `get_model()`    | `unwrap_or_else(\|e\| e.into_inner())` |
| Line 537-539 | `model_count()`  | `unwrap_or_else(\|e\| e.into_inner())` |

### Decision: No Change Needed

The code already uses Option B (recovery via `into_inner()`). This is the correct pattern for a cache:

- Cache corruption from a panic is non-fatal
- Reads can proceed with stale/partial data
- Writes will overwrite with fresh data
- Propagating errors for cache access adds complexity without benefit

The original issue report appears to have been based on outdated code.

## Issue 2: HTTP Client Construction

### Current State

**registry.rs:109-114** - Already has timeouts:

```rust
let client = reqwest::Client::builder()
    .timeout(HTTP_TIMEOUT)          // 30s
    .connect_timeout(HTTP_CONNECT_TIMEOUT)  // 10s
    .build()
    .unwrap_or_else(|_| reqwest::Client::new());
```

**models_dev.rs:45-49** - Also has timeouts:

```rust
let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(30))
    .connect_timeout(std::time::Duration::from_secs(10))
    .build()
    .unwrap_or_else(|_| reqwest::Client::new());
```

### Problem

Duplication: Both files define identical timeout values and client construction.

### Decision: Extract shared helper (Option B)

**Rationale:**

- Centralizes timeout policy
- Reduces code duplication
- Module-level lazy static (Option C) adds unnecessary complexity for clients used infrequently

### Implementation

**Add to `src/provider/mod.rs`:**

```rust
use std::time::Duration;

/// Default timeout for HTTP requests.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
pub const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Create an HTTP client with standard timeouts.
pub fn create_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}
```

**Update registry.rs:109-114:**

```rust
// Before
let client = reqwest::Client::builder()
    .timeout(HTTP_TIMEOUT)
    .connect_timeout(HTTP_CONNECT_TIMEOUT)
    .build()
    .unwrap_or_else(|_| reqwest::Client::new());

// After
let client = crate::provider::create_http_client();
```

Also remove `HTTP_TIMEOUT` and `HTTP_CONNECT_TIMEOUT` constants from registry.rs (lines 11-13).

**Update models_dev.rs:45-49:**

```rust
// Before
let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(30))
    .connect_timeout(std::time::Duration::from_secs(10))
    .build()
    .unwrap_or_else(|_| reqwest::Client::new());

// After
let client = crate::provider::create_http_client();
```

## Issue 3: Duplicated Filter Logic

### Current State

**list_models() (lines 414-474)** - Reads from cache, applies full filtering:

- min_context
- require_tools
- require_vision
- max_input_price
- id_prefix
- **prefs.ignore** (provider blacklist)
- **prefs.only** (provider whitelist)
- Calls `sort_models()`

**list_models_from_vec() (lines 363-411)** - Takes external models, applies partial filtering:

- min_context
- require_tools
- require_vision
- max_input_price
- id_prefix
- **MISSING: prefs.ignore**
- **MISSING: prefs.only**
- Calls `sort_models()`

### Problem

1. **Bug**: `list_models_from_vec()` ignores `prefs.ignore` and `prefs.only`
2. **Duplication**: 55 lines of nearly identical filter predicates

### Decision: Extract filter predicate function

### Implementation

**Add filter predicate (after `sort_models`, around line 528):**

```rust
/// Check if a model passes the filter criteria.
fn model_matches_filter(model: &ModelInfo, filter: &ModelFilter, prefs: &ProviderPrefs) -> bool {
    // Min context check
    if let Some(min) = filter.min_context {
        if model.context_window < min {
            return false;
        }
    }

    // Tool support check
    if filter.require_tools && !model.supports_tools {
        return false;
    }

    // Vision support check
    if filter.require_vision && !model.supports_vision {
        return false;
    }

    // Max input price check
    if let Some(max) = filter.max_input_price {
        if model.pricing.input > max {
            return false;
        }
    }

    // ID prefix check
    if let Some(ref prefix) = filter.id_prefix {
        if !model.id.to_lowercase().contains(&prefix.to_lowercase()) {
            return false;
        }
    }

    // Provider ignore list
    if let Some(ref ignore) = prefs.ignore {
        if ignore.iter().any(|p| p.eq_ignore_ascii_case(&model.provider)) {
            return false;
        }
    }

    // Provider only list
    if let Some(ref only) = prefs.only {
        if !only.iter().any(|p| p.eq_ignore_ascii_case(&model.provider)) {
            return false;
        }
    }

    true
}
```

**Refactor list_models_from_vec():**

```rust
/// List models matching filter criteria from a provided list.
pub fn list_models_from_vec(
    &self,
    models: Vec<ModelInfo>,
    filter: &ModelFilter,
    prefs: &ProviderPrefs,
) -> Vec<ModelInfo> {
    let mut filtered: Vec<ModelInfo> = models
        .into_iter()
        .filter(|m| model_matches_filter(m, filter, prefs))
        .collect();

    self.sort_models(&mut filtered, filter, prefs);
    filtered
}
```

**Refactor list_models():**

```rust
/// List models matching filter criteria.
pub fn list_models(&self, filter: &ModelFilter, prefs: &ProviderPrefs) -> Vec<ModelInfo> {
    let cache = self.cache.read().unwrap_or_else(|e| e.into_inner());
    let mut models: Vec<ModelInfo> = cache
        .models
        .iter()
        .filter(|m| model_matches_filter(m, filter, prefs))
        .cloned()
        .collect();

    self.sort_models(&mut models, filter, prefs);
    models
}
```

## Implementation Order

1. **Issue 2 (HTTP client)** - No dependencies, pure extraction
2. **Issue 3 (filter logic)** - Bug fix + deduplication

Issue 1 requires no changes.

## Implementation Checklist

- [x] Add `create_http_client()` to `src/provider/mod.rs`
- [x] Add `HTTP_TIMEOUT` and `HTTP_CONNECT_TIMEOUT` constants to `src/provider/mod.rs`
- [x] Update `registry.rs` to use `crate::provider::create_http_client()`
- [x] Remove local timeout constants from `registry.rs`
- [x] Update `models_dev.rs` to use `crate::provider::create_http_client()`
- [x] Add `model_matches_filter()` function to `registry.rs`
- [x] Refactor `list_models_from_vec()` to use `model_matches_filter()`
- [x] Refactor `list_models()` to use `model_matches_filter()`
- [x] Run `cargo test` to verify no regressions
- [x] Run `cargo clippy` to verify no new warnings

## Risk Assessment

| Change                                       | Risk   | Mitigation                             |
| -------------------------------------------- | ------ | -------------------------------------- |
| HTTP client extraction                       | Low    | Pure refactor, behavior unchanged      |
| Filter function extraction                   | Low    | Behavior preserved, bug fixed          |
| Adding ignore/only to `list_models_from_vec` | Medium | May filter out previously shown models |

The filter bug fix (adding ignore/only to `list_models_from_vec`) is the only behavioral change. Callers may see fewer models if they have `prefs.ignore` or `prefs.only` set. This is the correct behavior - the current code is a bug.

## Files Changed

| File                         | Changes                                           |
| ---------------------------- | ------------------------------------------------- |
| `src/provider/mod.rs`        | Add `create_http_client()`, constants             |
| `src/provider/registry.rs`   | Use shared client helper, extract filter function |
| `src/provider/models_dev.rs` | Use shared client helper                          |
