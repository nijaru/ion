# Sprint 16 Correctness Review

**Date:** 2026-02-08
**Scope:** Commits cf9ca9c..3fc9d7c (subagents, hooks, MCP lazy loading)
**Build:** clean | Tests: 390 pass, 0 fail | Clippy: 0 warnings

## Critical (must fix)

### [ERROR] /Users/nick/github/nijaru/ion/src/tool/mod.rs:55-74 -- MCP fallback bypasses permission checks (confidence: 95%)

MCP tools called through the fallback path skip the `PermissionMatrix` check entirely. Built-in tools are checked at lines 108-114. MCP tools have `DangerLevel::Restricted` (mcp/mod.rs:197), so in Read mode they should be blocked -- but the fallback returns before reaching the permission check.

This is a security regression: before Sprint 16, MCP tools were registered individually as `McpTool` objects and went through the normal `check_permission()` path.

```rust
// src/tool/mod.rs:55-74 -- returns early, never reaches line 108-114
if !self.tools.contains_key(name)
    && let Some(ref mcp) = self.mcp_fallback
    && let Some(result) = mcp.call_tool_by_name(name, args.clone()).await
{
    // No permission check here
    let mcp_result = result?;
    // ... only PostToolUse hooks ...
    return ...;
}
```

-> Add permission check before calling `mcp.call_tool_by_name()`. Since there is no `Tool` trait object to pass to `check_permission`, check the mode directly: if Read mode, return `PermissionDenied` for all MCP fallback calls (they are all Restricted).

### [ERROR] /Users/nick/github/nijaru/ion/src/tool/mod.rs:55-74 -- MCP fallback skips PreToolUse hooks (confidence: 95%)

Same code path. PreToolUse hooks (which can reject, modify, or skip tool calls) are not executed for MCP fallback tools. A user with a `pre_tool_use` hook to audit or block certain tools will find MCP tools completely unprotected.

-> Run PreToolUse hook dispatch before `mcp.call_tool_by_name()`, using the same pattern as lines 83-106.

### [ERROR] /Users/nick/github/nijaru/ion/src/tool/builtin/spawn_subagent.rs:39 -- Wrong path in tool description (confidence: 99%)

The LLM-facing parameter description says `~/.config/agents/subagents/` but the actual path is `~/.agents/subagents/` per `config::subagents_dir()` (config/mod.rs:358-360).

```rust
"description": "Name of the subagent to spawn (from ~/.config/agents/subagents/)"
```

-> Fix to `"Name of the subagent to spawn (e.g. 'explorer', 'planner')"`.

## Important (should fix)

### [WARN] /Users/nick/github/nijaru/ion/src/hook/mod.rs:199 -- Invalid regex silently becomes no-filter hook (confidence: 95%)

`regex::Regex::new(p).ok()` silently drops invalid regex patterns. The `tool_pattern` becomes `None`, so the hook fires for ALL tools. A user writes `tool_pattern = "write[edit"` expecting the hook to only fire for write/edit, but it fires for everything.

```rust
let tool_pattern = tool_pattern.and_then(|p| regex::Regex::new(p).ok());
```

-> Log `tracing::warn!("Invalid regex in hook tool_pattern: {p}")` and either reject the hook (return `None`) or keep it with no filter and the warning.

### [WARN] /Users/nick/github/nijaru/ion/src/agent/subagent.rs:154 -- Subagents hardcode Write mode (confidence: 90%)

`run_subagent` creates its orchestrator with `ToolMode::Write` regardless of the parent's mode. If the parent agent is in Read mode, subagents can still write files via their whitelisted tools.

```rust
let mut orchestrator = ToolOrchestrator::with_builtins(ToolMode::Write);
```

The default subagents (explorer, planner) only have read-safe tools, so this is mitigated for defaults. But user-defined YAML subagents with `write` or `bash` tools would have full write access even when the parent is in Read mode.

-> Accept a `mode: ToolMode` parameter in `run_subagent` and propagate the parent's mode.

### [WARN] /Users/nick/github/nijaru/ion/src/hook/mod.rs:232-240 -- Hook timeout does not kill child process (confidence: 85%)

When the 10-second `tokio::time::timeout` fires, the `Command::output()` future is dropped but the spawned `sh -c` process is not killed. `tokio::process::Child` does not kill on drop by default. The hook process continues running as an orphan.

```rust
match tokio::time::timeout(std::time::Duration::from_secs(10), result).await {
    // ...
    Err(_) => HookResult::Abort(format!("Hook timed out: {}", self.command)),
}
```

-> Use `Command::spawn()` to get a `Child` handle, then explicitly `child.kill()` on timeout. Alternatively, use `child.kill_on_drop(true)`.

### [WARN] /Users/nick/github/nijaru/ion/src/cli.rs:329-337 -- CLI silently ignores invalid hook events (confidence: 95%)

Invalid hook events are silently skipped in CLI mode. The TUI path (setup.rs:152-154) logs an error for the same case.

```rust
// CLI: no error log
if let Some(hook) = crate::hook::CommandHook::from_config(...) {
    orch.register_hook(Arc::new(hook)).await;
}
// vs TUI: has error log
} else {
    error!("Invalid hook event '{}', ...", hook_cfg.event);
}
```

-> Add `else { tracing::error!(...) }` branch matching setup.rs.

### [WARN] /Users/nick/github/nijaru/ion/src/config/mod.rs:318-320 -- Hooks accumulate across config layers with no dedup (confidence: 90%)

`hooks.extend()` adds hooks from each config layer additively. If the same hook is defined in `~/.ion/config.toml` AND `.ion/config.toml`, it runs twice. This is arguably the correct design (additive like `mcp_servers`), but since hooks execute shell commands, running the same command twice per tool call could have side effects.

This is a design choice, not a bug. But it differs from most other config fields which use override semantics.

-> Document this behavior. Consider whether hooks should have an identifier for dedup.

### [WARN] /Users/nick/github/nijaru/ion/src/mcp/mod.rs:300-321 -- `get_all_tools` is now dead code (confidence: 99%)

After lazy loading replaced eager registration, `McpManager::get_all_tools()` has zero callers.

-> Remove the dead method.

## Verified: No Issues Found

- **Build and tests**: `cargo build` and `cargo test` both clean (390 tests pass)
- **Clippy**: Zero warnings
- **Subagent defaults**: `with_defaults()` correctly registers explorer and planner with appropriate tool whitelists and max_turns values
- **Hook TOML parsing**: `[[hooks]]` array syntax parses correctly, tested via `test_hooks_config_parse`
- **Hook priority ordering**: `HookRegistry::register` sorts by priority after each insert, preserving registration order for equal priorities
- **MCP tool index**: `build_index()` correctly aggregates tools from all clients, `search_tools()` does case-insensitive matching, `call_tool_by_name()` routes to the correct client
- **McpToolsTool**: Properly validates `query` arg, handles empty results, formats output clearly
- **Config merge**: Hooks merge additively (`extend`), matching the pattern used by `mcp_servers`
- **SubagentRegistry user override**: User YAML configs loaded after defaults correctly override by name via HashMap insert
- **filter_tools**: Empty whitelist correctly keeps all tools (short-circuit return)
- **ToolOrchestrator.register_hook**: Uses `&self` with internal `RwLock`, correct for use on `Arc<ToolOrchestrator>`

## Summary

| Severity | Count | Themes                                                                    |
| -------- | ----- | ------------------------------------------------------------------------- |
| ERROR    | 3     | MCP fallback bypasses permissions + hooks; wrong path in tool description |
| WARN     | 6     | Silent failures, privilege escalation, process leak, inconsistency        |

The most impactful issues are the MCP fallback path bypassing both permission checks and PreToolUse hooks. These are regressions from the move to lazy loading -- the old eager registration path handled both correctly.
