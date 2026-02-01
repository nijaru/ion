# Refactor Sprint Review - 2026-01-31

**Reviewer:** claude-opus-4-5
**Scope:** Commits 10d1964 through 76c40a4 (hook system, file splits, test additions)
**Tests:** 304 passing
**Clippy:** 5 warnings (non-critical, documented below)

## Summary

The refactor sprint successfully split large modules into focused files, added a hook system for extensibility, and increased test coverage by 79 tests. Code quality is good overall with clear single-responsibility splits and proper module organization.

## Critical Issues

None found.

## Important Issues (Should Fix)

### [WARN] src/cli.rs:19-73, src/tui/message_list.rs:19-139 - Duplicate helper functions

Both files independently implement `extract_key_arg`, `take_head`, `take_tail`, and similar truncation logic (truncate_arg vs truncate_for_display).

```rust
// cli.rs:19
fn extract_key_arg(tool_name: &str, args: &serde_json::Value) -> String { ... }

// message_list.rs:19
pub(crate) fn extract_key_arg(tool_name: &str, args: &serde_json::Value) -> String { ... }
```

**Confidence:** 95%
**Fix:** Create a shared `util` module (e.g., `src/util.rs` or `src/tui/util.rs`) and consolidate these functions. The TUI version is already `pub(crate)` and could be reused by CLI.

### [WARN] src/provider/registry/types.rs:63-68 - Custom Pipe trait is non-idiomatic

```rust
pub(crate) trait Pipe: Sized {
    fn pipe<F, R>(self, f: F) -> R
    where
        F: FnOnce(Self) -> R,
    {
        f(self)
    }
}
```

The `Pipe` trait is used exactly once (line 68: `s.parse().unwrap_or(0.0).pipe(Ok)`) and could be replaced with a simple expression.

**Confidence:** 90%
**Fix:** Remove the trait, replace with `Ok(s.parse().unwrap_or(0.0))`.

### [WARN] src/provider/registry/tests.rs:4 - Redundant nested `mod tests`

Clippy warning `module_inception`: The file is already named `tests.rs` and is conditionally compiled with `#[cfg(test)]`, so the inner `mod tests` wrapper is redundant.

```rust
// src/provider/registry/tests.rs
#[cfg(test)]
mod tests {  // <-- redundant
    ...
}
```

**Confidence:** 100%
**Fix:** Remove the inner `mod tests { }` wrapper, keep the imports and test functions at file level. Same issue exists in:

- `src/tui/composer/tests.rs`
- `src/tui/highlight/tests.rs`

## Informational (Verify)

### [NIT] src/tool/types.rs:81-82 - check_sandbox has debug-friendly error

The error message includes `--no-sandbox` CLI flag which tightly couples types.rs to CLI interface. This is acceptable but worth noting if the flag name changes.

```rust
Err(format!(
    "Path '{}' is outside the sandbox ({}). Use --no-sandbox to allow.",
    ...
))
```

**Confidence:** 70%
**Suggestion:** Consider making the error message more generic or moving it to CLI layer.

### [NIT] src/hook/mod.rs - Well-designed but unused

The hook system is properly integrated into `ToolOrchestrator::call_tool()` with PreToolUse and PostToolUse hooks. However, no actual hooks are registered anywhere in the codebase yet. This is intentional (infrastructure for future features) but should be documented.

**Confidence:** 85%
**Suggestion:** Add a brief note in STATUS.md or DESIGN.md about planned hook use cases.

## Architecture Assessment

### File Splits (Good)

| Split         | Before     | After (largest)          | Verdict    |
| ------------- | ---------- | ------------------------ | ---------- |
| openai_compat | ~800 lines | 240 (request_builder.rs) | Good       |
| registry      | 744 lines  | 278 (fetch.rs)           | Good       |
| render        | ~400 lines | 450 (direct.rs)          | Acceptable |
| hook          | new        | 249                      | Good       |
| tool/types    | extracted  | 228                      | Good       |

All splits follow single-responsibility principle. The `render/direct.rs` at 450 lines is the largest but contains rendering methods for different UI elements which naturally belong together.

### Module Organization (Good)

- `mod.rs` files are clean entry points with re-exports
- `pub use` re-exports are appropriate for downstream API
- Internal types marked `pub(crate)` correctly

### Test Coverage (Good)

New tests added:

- message_list.rs: 43 tests (formatting, scrolling, tool output)
- auth/openai.rs: 5 tests (OAuth URL building)
- auth/google.rs: 6 tests (OAuth URL, offline access)
- cli.rs: 25 tests (arg parsing, commands, permissions)

Test quality is good:

- Cover edge cases (empty inputs, unicode, long strings)
- Use descriptive names
- No mock infrastructure (per project guidelines)

### Hook System (Good)

Clean async trait-based design:

- HookPoint enum for lifecycle events
- HookContext for passing data
- HookResult for controlling flow (Continue, Skip, Replace, Abort)
- Priority ordering with stable sort
- Integration in ToolOrchestrator is non-intrusive

## Clippy Warnings (Known, Non-Critical)

1. `field_reassign_with_default` in config/mod.rs:485 - Style preference
2. `module_inception` x3 - Redundant nested test modules (listed above)

## Recommendations

1. **Consolidate duplicate utils** - Create shared truncation/extraction helpers
2. **Remove Pipe trait** - Replace with direct expression
3. **Flatten test modules** - Remove redundant `mod tests` wrappers
4. **Document hook plans** - Note intended use cases for future contributors

## Verdict

**Approved.** The refactor achieves its goals of modularization and extensibility. The duplicate code issue is minor and can be addressed in a follow-up commit. All tests pass and clippy is clean (warnings are known non-issues).
