# Correctness Review: Security Fix Commits

**Commits:** `9ee61d5` (fixes), `d00c5ca` (tests + McpFallback trait)
**Scope:** 9 files, ~330 lines
**Date:** 2026-02-08

## Summary

The fixes address three issues from the Sprint 16 review:

1. MCP fallback bypassed permission checks and hooks
2. Project configs could inject shell command hooks
3. Subagents hardcoded ToolMode::Write (privilege escalation)

All three fixes are logically correct within their scope. Tests are well-targeted and pass (398/398). One residual issue with runtime mode toggling.

## Findings

### WARN: Subagent mode is a snapshot, not live

**File:** `/Users/nick/github/nijaru/ion/src/tool/builtin/spawn_subagent.rs:14`

`SpawnSubagentTool` stores `mode: ToolMode` as a `Copy` field set at construction time. When the user toggles mode at runtime via Shift+Tab (`src/tui/events.rs:272`), the orchestrator's `PermissionMatrix` updates but `SpawnSubagentTool.mode` does not. If a user starts in Write mode and toggles to Read mode, subagents spawned afterward still run in Write mode.

This is a pre-existing design issue (the fix correctly propagates the _initial_ mode), but the fix creates an expectation that mode propagation works, which it only does at startup.

```rust
// spawn_subagent.rs:14 -- frozen at construction
mode: ToolMode,

// events.rs:280 -- updates orchestrator but not SpawnSubagentTool
orchestrator.set_tool_mode(mode).await;
```

-> Fix: Query the orchestrator's mode at execution time instead of storing it. Either pass the orchestrator to run_subagent, or have SpawnSubagentTool read mode from the orchestrator when execute() is called.

---

## Verified Correct

### MCP fallback permission check (src/tool/mod.rs:56-114)

The new MCP fallback path correctly:

- Checks `has_tool` before attempting the call (avoids unnecessary work)
- Reads mode from `PermissionMatrix` and blocks in Read mode (line 61-66)
- Runs `PreToolUse` hooks with the same pattern as the builtin path (lines 68-92)
- Handles all `HookResult` variants: Continue, Skip, ReplaceInput, ReplaceOutput, Abort
- Handles `call_tool_by_name` returning `None` gracefully (line 94-98)
- Runs `PostToolUse` hooks after execution (lines 101-114)
- No TOCTOU race: `McpManager.tool_index` is immutable after `build_index()`

### Project hooks stripping (src/config/mod.rs:225-242)

The snapshot/restore pattern correctly covers both project layers:

1. User-global hooks loaded (layer 1)
2. Snapshot via `mem::take` (line 227) -- moves hooks out, leaves empty vec
3. Project shared config merges (layer 2) -- any hooks go into the now-empty vec
4. Project local config merges (layer 3) -- same
5. Restore user-only hooks (line 242) -- discards everything from steps 3-4

The `merge()` method uses `extend` for hooks (line 326), so project hooks accumulate in `config.hooks` during steps 3-4. The final assignment at step 5 replaces them entirely.

### Hook regex rejection (src/hook/mod.rs:199-207)

Invalid regex now causes `from_config` to return `None`, which callers handle:

- `cli.rs:333-344` logs an error
- `setup.rs:147-155` logs an error

Both callers already had the `if let Some(hook)` pattern; the fix just makes it return `None` for bad regex instead of silently accepting a non-matching hook.

### Tool pattern None handling (src/hook/mod.rs:225-231)

The old code had a logic inversion:

```rust
// OLD (wrong): hook with pattern fires when tool_name is None
if let Some(ref pattern) = self.tool_pattern
    && let Some(ref tool_name) = ctx.tool_name
    && !pattern.is_match(tool_name)
{
    return HookResult::Continue;  // skips only when BOTH exist AND no match
}
// When tool_name is None, the outer `let` chain fails, falls through to execute
```

The new code correctly skips when there's a pattern but no tool_name:

```rust
// NEW (correct):
if let Some(ref pattern) = self.tool_pattern {
    match ctx.tool_name {
        Some(ref tool_name) if pattern.is_match(tool_name) => {}  // match -> proceed
        _ => return HookResult::Continue,  // no match or no name -> skip
    }
}
```

### kill_on_drop (src/hook/mod.rs:242-257)

Splitting `.output()` into `.spawn()` + `.wait_with_output()` with `.kill_on_drop(true)` ensures the child process is killed if the timeout fires and the future is dropped. The old `.output()` approach would leave orphan processes on timeout.

### Subagent mode propagation (src/agent/subagent.rs:147)

Both callers updated:

- `cli.rs:323-327` passes `permissions.mode`
- `setup.rs:137-141` passes `permissions.mode`

No other callers of `run_subagent` or `SpawnSubagentTool::new` exist.

### RwLock removal (spawn_subagent.rs)

`SubagentRegistry` is immutable after construction (loaded once, then wrapped in `Arc`). Removing the `RwLock` is correct -- no code path mutates it after initialization.

### ToolError Clone derive (src/tool/types.rs:140)

All variants contain only `String`, so `Clone` is trivially derivable. Added to support `MockMcpFallback` in tests where `result: Option<Result<ToolResult, ToolError>>` needs to be cloned across async boundaries. No behavioral change for production code.

## Test Assessment

| Test                                                   | Verifies                                 | Correct |
| ------------------------------------------------------ | ---------------------------------------- | ------- |
| test_mcp_fallback_blocked_in_read_mode                 | MCP tools blocked in Read mode           | Yes     |
| test_mcp_fallback_allowed_in_write_mode                | MCP tools work in Write mode             | Yes     |
| test_mcp_fallback_runs_pre_hooks                       | PreToolUse abort prevents MCP call       | Yes     |
| test_mcp_fallback_skip_hook                            | PreToolUse skip returns synthetic result | Yes     |
| test_project_hooks_stripped                            | Snapshot/restore discards project hooks  | Yes     |
| test_command_hook_invalid_regex_rejected               | Bad regex -> from_config returns None    | Yes     |
| test_command_hook_pattern_skips_when_tool_name_none    | Pattern + no tool_name -> Continue       | Yes     |
| test_command_hook_no_pattern_fires_when_tool_name_none | No pattern + no tool_name -> fires       | Yes     |

All tests target the specific bug or security issue they claim to verify. The mock infrastructure (MockMcpFallback, AbortHook, SkipHook) is minimal and correctly exercises the code paths.

## Missing Test Coverage

- No test for `ReplaceInput` hook variant on MCP path (hooks replace args before MCP call)
- No test for `ReplaceOutput` hook variant on MCP path (post-hook replaces output)
- No test for runtime mode toggle + subagent interaction (the WARN above)

These are lower priority since the hook dispatch logic is shared with the well-tested builtin path.
