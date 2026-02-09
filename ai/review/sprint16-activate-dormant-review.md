# Sprint 16 "Activate Dormant Infrastructure" Review

**Date:** 2026-02-08
**Commits:** cf9ca9c..3fc9d7c (3 commits, +563/-40 lines)
**Build:** clean, 390 tests pass, 0 clippy warnings

## Summary

Three features activating previously-stubbed infrastructure:

1. Default subagents (explorer, planner) via `SubagentRegistry::with_defaults()`
2. Config-driven hooks: `[[hooks]]` TOML -> `CommandHook` structs
3. MCP lazy loading: tool index + `mcp_tools` search tool + fallback in `ToolOrchestrator`

Overall quality is solid. Code is well-structured, tests cover the new functionality, and it follows existing codebase patterns. Issues below.

## Critical (must fix)

### [ERROR] src/tool/mod.rs:55-74 - MCP fallback skips PreToolUse hooks

The MCP fallback path runs `PostToolUse` hooks but skips `PreToolUse` hooks entirely. Built-in tools go through both pre and post hooks. This means a `pre_tool_use` hook configured to block dangerous operations (e.g., abort on specific tool names) can be completely bypassed by any MCP tool.

```rust
// MCP fallback path - no PreToolUse hooks!
if !self.tools.contains_key(name)
    && let Some(ref mcp) = self.mcp_fallback
    && let Some(result) = mcp.call_tool_by_name(name, args.clone()).await
{
    // Only PostToolUse is run
```

-> Run `PreToolUse` hooks before `mcp.call_tool_by_name()`, same as the builtin path.

### [ERROR] src/tool/builtin/spawn_subagent.rs:39 - Tool description has wrong path

The parameter description tells the LLM subagents are loaded from `~/.config/agents/subagents/` but the actual path is `~/.agents/subagents/` (see `config::subagents_dir()`). The LLM will direct users to the wrong directory.

```rust
"description": "Name of the subagent to spawn (from ~/.config/agents/subagents/)"
```

-> Change to `"Name of the subagent to spawn (e.g. 'explorer', 'planner')"` or reference the correct `~/.agents/subagents/` path.

## Important (should fix)

### [WARN] src/hook/mod.rs:199 - Invalid regex silently ignored

If a user provides an invalid regex in `tool_pattern`, `Regex::new(p).ok()` silently discards it, and the hook runs on ALL tools (no filter). The user likely wanted to restrict the hook, so this is a surprising failure mode.

```rust
let tool_pattern = tool_pattern.and_then(|p| regex::Regex::new(p).ok());
```

-> Either log a warning (`tracing::warn!`) or return `None` from `from_config()` to reject the hook entirely.

### [WARN] src/hook/mod.rs:216-221 - Hook executes when tool_name is None despite having a filter

When `tool_pattern` is `Some` but `ctx.tool_name` is `None`, the let-chain short-circuits and the hook runs (falls through to execute). A hook configured for specific tools should not fire when the tool name is unknown.

```rust
if let Some(ref pattern) = self.tool_pattern
    && let Some(ref tool_name) = ctx.tool_name
    && !pattern.is_match(tool_name)
{
    return HookResult::Continue;
}
```

-> Add a separate check: if `tool_pattern` is `Some` and `tool_name` is `None`, return `Continue`.

### [WARN] src/mcp/mod.rs:300-321 - `get_all_tools` is dead code

After lazy loading was introduced, `get_all_tools` has zero callers. The previous eager registration code in `setup.rs` was the only caller and was replaced.

-> Remove `McpManager::get_all_tools()` to keep the API surface clean.

### [WARN] src/agent/subagent.rs + cli.rs + setup.rs - SubagentRegistry wrapped in RwLock but never written after construction

`SubagentRegistry` is constructed, populated, then wrapped in `Arc<RwLock<SubagentRegistry>>`. Only `.read()` is ever called (in `SpawnSubagentTool::execute`). The `RwLock` adds unnecessary async contention overhead.

```rust
// cli.rs:320 and setup.rs:134
let subagent_registry = Arc::new(tokio::sync::RwLock::new(subagent_registry));
```

-> Use `Arc<SubagentRegistry>` instead. Change `SpawnSubagentTool` field from `Arc<RwLock<SubagentRegistry>>` to `Arc<SubagentRegistry>`.

### [WARN] src/cli.rs:329-337 - CLI silently ignores invalid hook events

When `CommandHook::from_config` returns `None` for an invalid event string, the CLI silently skips it. The TUI path (setup.rs:152) logs an error. Inconsistent behavior.

```rust
// CLI: silent skip
if let Some(hook) = crate::hook::CommandHook::from_config(...) {
    orch.register_hook(Arc::new(hook)).await;
}

// TUI: logs error
} else {
    error!("Invalid hook event '{}', ...", hook_cfg.event);
}
```

-> Add the same `else { error!(...) }` branch in CLI mode.

## Nits

### [NIT] src/mcp/mod.rs:215 - Extra blank line

Double blank line between `McpToolEntry` and `McpManager` impl block (line 215).

### [NIT] src/mcp/mod.rs:256-270 - search_tools allocates lowercased strings per entry

`search_tools` calls `.to_lowercase()` on every entry's name and description per search call. For small indexes this is fine, but if tool counts grow, consider pre-computing lowered strings in the index.

## Design Notes (informational)

- **Hook accumulation across config layers**: `hooks.extend()` means global + project + local hooks all stack. This is the right behavior (matches `mcp_servers`), but worth documenting since most other config fields use override semantics.

- **MCP not available in CLI mode**: CLI `setup_cli_agent` does not initialize MCP servers. This is a pre-existing gap (not introduced in Sprint 16) but may surprise users running `ion run` with MCP configs.

- **No permission check on MCP fallback tools**: MCP tools called via fallback bypass the `PermissionMatrix` check that built-in tools go through. The `McpTool` type has `DangerLevel::Restricted`, but in the fallback path `call_tool_by_name` does not check permissions. All MCP tools are tagged `Restricted`, so in `Read` mode they should be blocked but currently are not via the fallback path.
