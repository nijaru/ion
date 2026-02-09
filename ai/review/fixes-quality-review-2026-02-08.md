# Quality Review: Security Fix Commits

**Date:** 2026-02-08
**Commits:** `9ee61d5` Fix review findings, `d00c5ca` Add tests + extract McpFallback trait
**Scope:** 9 files, ~330 lines
**Verdict:** Solid security fixes with good test coverage. Two concrete issues found.

## Summary

The changes fix three security issues (MCP permission bypass, hook injection from
project configs, subagent privilege escalation) and add a `McpFallback` trait to
make the MCP path testable. The fixes are correct and well-tested.

## Findings

### WARN: Dead code -- McpTool struct has no callers

`/Users/nick/github/nijaru/ion/src/mcp/mod.rs:177-212`

The `McpTool` struct and its `impl Tool for McpTool` block are dead code. The only
caller was `get_all_tools()`, which was removed in this diff. The struct is `pub`
so the compiler does not warn, but nothing instantiates it. The MCP fallback path
now goes through `McpManager::call_tool_by_name` (via the `McpFallback` trait)
which uses `McpToolEntry` internally.

```rust
// Dead -- never instantiated after get_all_tools removal
pub struct McpTool {
    pub client: Arc<McpClient>,
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
```

-> Delete `McpTool` struct and its `impl Tool for McpTool` (lines 177-212).

### WARN: Duplicated hook dispatch logic between MCP and builtin paths

`/Users/nick/github/nijaru/ion/src/tool/mod.rs:68-92` vs `122-146`

The PreToolUse hook dispatch is copy-pasted between the MCP fallback path (lines
68-92) and the builtin tool path (lines 122-146). Both are identical 25-line match
blocks. The PostToolUse dispatch is also duplicated (lines 101-114 vs 161-174),
though the MCP path uses a wildcard `_ =>` while the builtin path explicitly
enumerates `Continue | Skip | ReplaceInput(_)`.

This is acceptable for now since extracting it would require returning a
three-state enum (proceed-with-args, return-ok, return-err), but worth noting as
a future refactor target. The behavioral difference in PostToolUse handling (wildcard
vs explicit arms) is harmless since both do the same thing, but the explicit form
in the builtin path is better for exhaustiveness checking if new variants are added.

-> Consider extracting a `run_pre_hooks` helper. Low priority since the logic is
stable and the duplication is contained in one function.

### NIT: PostToolUse wildcard arm in MCP path

`/Users/nick/github/nijaru/ion/src/tool/mod.rs:113`

```rust
_ => Ok(mcp_result),
```

The builtin path (line 173) explicitly matches `Continue | Skip | ReplaceInput(_)`
for exhaustiveness. The MCP path uses `_`. If a new `HookResult` variant is added,
the compiler will catch it on the builtin path but silently accept it on the MCP path.

-> Change `_ => Ok(mcp_result)` to explicit arm matching for consistency.

## Positive Observations

**Security fixes are correct:**

- Config hook snapshot/restore pattern (config/mod.rs:227,242) cleanly prevents
  project configs from injecting hooks. The test at line 538 verifies it.
- MCP Read mode block (tool/mod.rs:60-66) is in the right place -- before hooks
  or tool execution. Test at line 337 validates it.
- Subagent mode inheritance (spawn_subagent.rs:14,78) closes privilege escalation.

**Hook refactoring is sound:**

- `CommandHook::from_config` now rejects invalid regex (hook/mod.rs:200-207)
  instead of silently treating it as "no pattern." Test at line 393.
- Tool pattern matching (hook/mod.rs:225-231) now correctly skips when tool_name
  is None. The old `let` chain would fire the hook when tool_name was None and
  the pattern didn't match, which was the opposite of intended behavior. Two tests
  at lines 399 and 409 cover both cases.
- `spawn` + `kill_on_drop` + `wait_with_output` (hook/mod.rs:242-257) is the
  correct pattern for timeout + cleanup.

**McpFallback trait is well-designed:**

- Minimal surface: `has_tool(&str) -> bool` and `call_tool_by_name(&str, Value) -> Option<Result>`.
- Enables MockMcpFallback in tests without any MCP server infrastructure.
- The four MCP tests (read-mode block, write-mode allow, pre-hook abort, pre-hook skip)
  cover all the critical paths.

**RwLock -> Arc for SubagentRegistry:**

- SubagentRegistry is only written during setup and read during execution. Dropping
  the RwLock removes unnecessary async overhead.

**ToolError derives Clone:**

- Required for MockMcpFallback's `result.clone()` in tests. Reasonable since all
  variants are String-based. No performance concern.

**Error logging improvements (cli.rs:317-321, 340-344):**

- Subagent load failures now logged instead of silently ignored.
- Invalid hook configs now logged with the invalid event string.

## Test Quality

Tests are clear, well-named, and test the right things:

- `test_mcp_fallback_blocked_in_read_mode` -- validates the security fix
- `test_mcp_fallback_runs_pre_hooks` -- validates hook dispatch for MCP
- `test_command_hook_invalid_regex_rejected` -- validates the regex fix
- `test_command_hook_pattern_skips_when_tool_name_none` -- validates pattern logic
- `test_project_hooks_stripped` -- validates hook injection prevention

The mock pattern (MockMcpFallback with canned results) is appropriate for unit tests.
No missing coverage for the security-critical paths.
