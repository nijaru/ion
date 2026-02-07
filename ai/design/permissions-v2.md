# Permissions & Extensibility v2

## Problem

Current system has 5 overlapping CLI flags (`-r`, `-w`, `-y`, `--no-sandbox`, `--agi`) with complex interactions. Research shows approval prompts are a failed security model: ~60% of users end up in YOLO mode, only 1.1% configure deny rules, and prompts cause 30-50 interruptions/hour.

## Design Principles

1. Everything runs by default. Sandbox provides real security, not prompts.
2. Two modes: Read (restricted) and Write (default, full access).
3. No approval system. Config-only deny rules for power users.
4. Fewer flags. Clean UX.

## Modes

Two modes: **Read** and **Write** (default).

| Mode  | File ops         | Bash               | Use case                 |
| ----- | ---------------- | ------------------ | ------------------------ |
| Read  | read, glob, grep | safe commands only | Code review, exploration |
| Write | all              | all                | Default: get work done   |

No prompts in either mode. Read mode blocks restricted ops. Write mode allows everything within sandbox.

## CLI Flags

```
ion                    # Write mode, sandboxed to CWD
ion --read             # Read-only mode
ion --no-sandbox       # Allow access outside CWD (combinable)
```

Two permission flags + one sandbox flag. All long-form, no short flags.

### Flag Resolution

| Flag           | mode                   | sandbox               |
| -------------- | ---------------------- | --------------------- |
| (none)         | config default (write) | config default (true) |
| `--read`       | read                   | (inherit)             |
| `--no-sandbox` | (inherit)              | false                 |

### What's Removed

| Old              | Why                                            |
| ---------------- | ---------------------------------------------- |
| `-r` (short)     | Long-form only: `--read`                       |
| `-w` / `--write` | Write is default. Not needed.                  |
| `-y` / `--yes`   | No approval system. Not needed.                |
| `--agi`          | Just `--no-sandbox` (write is already default) |

### Headless / CLI Mode

`ion run "prompt"` runs in write mode by default. `ion run --read "prompt"` for read-only. No `--yes` needed. No `DenyApprovalHandler` needed.

## Config

```toml
[permissions]
mode = "write"         # "read" or "write"
sandbox = true         # --no-sandbox equivalent

# Optional: commands blocked entirely (glob patterns)
deny_commands = [
    "sudo *",
    "rm -rf /",
]
```

### Config Layering (unchanged)

1. CLI flags (highest)
2. `.ion/config.local.toml` (project local, gitignored)
3. `.ion/config.toml` (project shared)
4. `~/.ion/config.toml` (global)
5. Built-in defaults (write mode, sandboxed)

### Deny Rules

Commands matched against `deny_commands` using glob patterns:

- `*` matches zero or more characters
- `?` matches exactly one character
- Matching checks each segment in piped/chained commands (`&&`, `||`, `;`, `|`)
- If ANY segment matches a deny pattern, the whole command is blocked

Optional config-only feature. Most users won't need it since the sandbox confines blast radius.

## TUI Mode Cycling

Shift+Tab cycles: `[READ]` <-> `[WRITE]`

Two modes. Status bar shows current mode.

## Sandbox

### Default Behavior

All file operations and bash commands restricted to CWD and subdirectories. Enforced at two levels:

1. **App-level**: `ToolContext::check_sandbox()` validates paths (current behavior)
2. **OS-level**: Bash child processes run in an OS sandbox (new)

### OS Sandbox Implementation

Sandbox bash child processes, not ion itself.

**macOS**: Spawn bash via `sandbox-exec -f <profile>`:

```scheme
(version 1)
(deny default)
(allow file-read* (subpath "/usr") (subpath "/bin") (subpath "/Library"))
(allow file-read* file-write* (subpath "<CWD>"))
(allow file-read* file-write* (subpath "/tmp"))
(allow process-exec)
(allow sysctl-read)
(allow mach-lookup)
```

**Linux**: Landlock via `landlock` crate. Restrict filesystem access to CWD + /tmp + system paths.

**Fallback**: When OS sandbox unavailable (old kernel, restricted env), fall back to app-level path checking only.

### --no-sandbox

Disables both OS sandbox and app-level path checking.

## What's Kept

- `is_safe_command()` in guard.rs — determines what bash can run in read mode
- `analyze_command()` in guard.rs — detects destructive patterns for display warnings
- `check_sandbox()` in ToolContext — app-level path checking
- `DangerLevel` enum (Safe/Restricted) — used by read mode to filter tools
- Hook system (PreToolUse/PostToolUse) — already implemented, orthogonal to permissions

## What's Removed

The entire approval system:

- `ApprovalHandler` trait
- `ApprovalResponse` enum (Yes/No/AlwaysSession/AlwaysPermanent)
- `PermissionStatus::NeedsApproval` variant
- `PermissionMatrix` session/permanent allowed sets (4 HashSets + 4 methods)
- `TuiApprovalHandler` struct
- `ApprovalRequest` struct
- `Mode::Approval` TUI modal state
- `AutoApproveHandler` / `DenyApprovalHandler` structs
- `ToolOrchestrator.approval_handler` field + setter
- `ToolOrchestrator.call_tool()` approval flow (lines 99-137)
- `pending_approval` field in TUI App
- `approval_rx` channel in TUI App
- `handle_approval_mode()` in events.rs
- `ToolMode::Agi` variant
- `PermissionSettings.auto_approve` field
- `PermissionSettings.agi_enabled` field
- `PermissionConfig.auto_approve` config field

## Extensibility

### Current State

| Layer           | Status                     | LOC                       |
| --------------- | -------------------------- | ------------------------- |
| AGENTS.md       | Done                       | ~100 (instruction loader) |
| Skills/SKILL.md | Done                       | ~590                      |
| MCP client      | Done (basic)               | ~240                      |
| Hooks           | Partial (framework exists) | —                         |

### MCP Context Management

Problem: MCP tools consume context. 50+ tools = ~72K tokens before work starts.

Solution: **Tool search** (progressive disclosure for MCP tools).

1. MCP servers start at session begin
2. Tool descriptions NOT included in system prompt
3. `tool_search` meta-tool describes available MCP tools on demand
4. Skills reference MCP tools and explain when/how to use them
5. When a skill activates, its referenced MCP tools get included in context

### Hooks (Next)

Shell commands triggered by lifecycle events. Config-driven:

```toml
[[hooks]]
event = "PostToolUse"
match = "write|edit"
command = "cargo fmt -- {file}"

[[hooks]]
event = "PreToolUse"
match = "bash"
command = "echo '{command}' | safety-check"
# exit 0 = proceed, exit 2 = block
```

Events: PreToolUse, PostToolUse, SessionStart, Stop, PreCompact.

### Plugin Packaging (Later)

Manifest bundling skills + MCP config + hooks. Install via `ion plugin add <git-url>`.

### What NOT to Build

- Embedded JS/TS runtime (+30-50MB binary, MCP covers this)
- WASM plugin host (premature, no ecosystem)
- Extension marketplace (git URLs sufficient)

## Migration: Complete File-by-File Changes

### src/tool/types.rs

| Change                         | Lines   | Detail                                                             |
| ------------------------------ | ------- | ------------------------------------------------------------------ |
| Remove `ApprovalResponse` enum | 156-162 | 4 variants, entire enum                                            |
| Remove `ApprovalHandler` trait | 164-168 | trait + async method                                               |
| Remove `ToolMode::Agi`         | 179     | Keep Read and Write (Write stays as default)                       |
| Update `ToolMode` doc          | 178     | "Full access, all tools allowed" instead of "Standard interactive" |

### src/tool/permissions.rs

| Change                                    | Lines  | Detail                                                                                                       |
| ----------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------ |
| Remove 4 HashSets from `PermissionMatrix` | 9-14   | `session_allowed_tools`, `permanent_allowed_tools`, `session_allowed_commands`, `permanent_allowed_commands` |
| Remove 4 methods                          | 38-54  | `allow_session`, `allow_permanently`, `allow_command_session`, `allow_command_permanently`                   |
| Remove `PermissionStatus::NeedsApproval`  | 111    | Only `Allowed` and `Denied` remain                                                                           |
| Simplify `check_permission()`             | 57-78  | Read: safe=Allowed, restricted=Denied. Write: always Allowed.                                                |
| Simplify `check_command_permission()`     | 80-105 | Read: safe list check. Write: always Allowed.                                                                |
| Remove `ToolMode::Agi` branches           | 60, 84 | Remove two match arms                                                                                        |
| Add optional `deny_commands`              | new    | Glob matching against deny list, returns Denied                                                              |

### src/tool/mod.rs

| Change                                         | Lines   | Detail                                                                                                                                            |
| ---------------------------------------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| Remove `approval_handler` field                | 17      | `Option<Arc<dyn ApprovalHandler>>`                                                                                                                |
| Remove `set_approval_handler()`                | 32-34   | Method                                                                                                                                            |
| Remove entire `NeedsApproval` branch           | 99-137  | 38 lines of approval flow + TODO                                                                                                                  |
| Simplify `call_tool()`                         | 97-140  | Only `Allowed` -> execute, `Denied` -> error                                                                                                      |
| Remove 3 tests                                 | 296-326 | `test_permission_matrix_write_restricted_needs_approval`, `test_permission_matrix_write_restricted_allowed_session`, `test_permission_matrix_agi` |
| Add `test_permission_matrix_write_all_allowed` | new     | Write mode allows everything                                                                                                                      |

### src/cli.rs

| Change                                                | Lines   | Detail                                                                                                                                  |
| ----------------------------------------------------- | ------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Remove `auto_approve` from `PermissionSettings`       | 27      | Field                                                                                                                                   |
| Remove `agi_enabled` from `PermissionSettings`        | 31      | Field                                                                                                                                   |
| Remove `-r` short flag on `read_mode`                 | 114     | Keep `--read` long only                                                                                                                 |
| Remove `-w` / `--write` flag entirely                 | 117-118 | Write is default, no flag needed                                                                                                        |
| Remove `-y` / `--yes` flag entirely                   | 121-122 | No approval system                                                                                                                      |
| Remove `--agi` flag entirely                          | 129-130 | Use `--no-sandbox` instead                                                                                                              |
| Simplify `resolve_permissions()`                      | 54-96   | Just: config defaults -> `--read` overrides mode -> `--no-sandbox` overrides sandbox                                                    |
| Remove `auto_approve` field from `Cli` struct         | ~122    | Public field used by main.rs                                                                                                            |
| Remove `AutoApproveHandler`                           | 266-272 | Struct + impl                                                                                                                           |
| Remove `DenyApprovalHandler`                          | 275-282 | Struct + impl                                                                                                                           |
| Simplify `setup_cli_agent()`                          | 315-389 | Remove `auto_approve` param, remove handler setup. Just: read mode -> ToolMode::Read, else -> ToolMode::Write                           |
| Simplify `run()` / `run_inner()`                      | 458-482 | Remove `auto_approve` param                                                                                                             |
| Update tests                                          | 714-879 | Remove `test_parse_auto_approve`, `test_parse_agi_mode`, update `test_parse_no_sandbox`, `test_permission_defaults`, simplify remaining |
| Remove `use ApprovalHandler, ApprovalResponse` import | 7       |                                                                                                                                         |

### src/main.rs

| Change                                      | Lines | Detail                      |
| ------------------------------------------- | ----- | --------------------------- |
| Remove `cli.auto_approve` from `run()` call | 14    | `ion::cli::run(args).await` |

### src/config/mod.rs

| Change                                        | Lines   | Detail      |
| --------------------------------------------- | ------- | ----------- |
| Remove `auto_approve` from `PermissionConfig` | 16      | Field       |
| Remove `ToolMode::Agi` from `mode()` match    | 27      | Match arm   |
| Remove `auto_approve` merge in `merge()`      | 283-284 | Merge logic |
| Add `deny_commands: Option<Vec<String>>`      | new     | New field   |
| Add `deny_commands` merge logic               | new     |             |
| Update tests                                  | varies  |             |

### src/tui/types.rs

| Change                                     | Lines   | Detail                                   |
| ------------------------------------------ | ------- | ---------------------------------------- |
| Remove `use crate::tool::ApprovalResponse` | 3       | Import                                   |
| Remove `Mode::Approval` variant            | 64-65   | Enum variant                             |
| Remove `ApprovalRequest` struct            | 166-171 | Struct with tool_name, args, response_tx |
| Remove `TuiApprovalHandler` struct         | 182-201 | Struct + `ApprovalHandler` impl          |

### src/tui/mod.rs

| Change                          | Lines  | Detail           |
| ------------------------------- | ------ | ---------------- |
| Remove `ApprovalRequest` import | 36, 59 | Two import lines |
| Remove `approval_rx` field      | 91     | Channel receiver |
| Remove `pending_approval` field | 94     | Optional request |

### src/tui/session/setup.rs

| Change                                            | Lines | Detail                                                                    |
| ------------------------------------------------- | ----- | ------------------------------------------------------------------------- |
| Remove `TuiApprovalHandler` import                | 21    | Import                                                                    |
| Remove `approval_tx/approval_rx` channel creation | 85    | `mpsc::channel(100)`                                                      |
| Remove approval handler setup block               | 88-92 | `if !permissions.auto_approve { orchestrator.set_approval_handler(...) }` |
| Remove `approval_rx` from App init                | 203   | Field init                                                                |
| Remove `pending_approval: None` from App init     | 206   | Field init                                                                |

### src/tui/session/update.rs

| Change                        | Lines   | Detail                                                                                |
| ----------------------------- | ------- | ------------------------------------------------------------------------------------- |
| Remove approval polling block | 161-166 | `if self.pending_approval.is_none() && let Ok(request) = self.approval_rx.try_recv()` |

### src/tui/events.rs

| Change                                 | Lines   | Detail                                                                     |
| -------------------------------------- | ------- | -------------------------------------------------------------------------- |
| Remove `use ApprovalResponse` import   | 4       | Import                                                                     |
| Remove `Mode::Approval` match arm      | 26      | In key event dispatch                                                      |
| Simplify mode cycling                  | 264-275 | Remove `agi_enabled` check, remove `ToolMode::Agi`. Just: `Read <-> Write` |
| Remove `handle_approval_mode()` method | 510-527 | Entire method (y/n/a/A key handling)                                       |

### src/tui/render/direct.rs

| Change                           | Lines | Detail                                              |
| -------------------------------- | ----- | --------------------------------------------------- |
| Remove `ToolMode::Agi` match arm | 443   | Status bar rendering                                |
| Update Write label               | 442   | Keep as `("WRITE", CColor::Yellow)` or adjust color |

### src/agent/subagent.rs

| Change                 | Lines | Detail                                        |
| ---------------------- | ----- | --------------------------------------------- |
| Keep `ToolMode::Write` | 118   | Already correct (subagents run in write mode) |

### src/agent/tools.rs

| Change                         | Lines | Detail          |
| ------------------------------ | ----- | --------------- |
| Keep `no_sandbox` from session | 29    | Already correct |

### src/session/mod.rs

| Change | — | No changes needed. `no_sandbox` field stays. |

### src/tool/builtin/guard.rs

| Change | — | No changes needed. `is_safe_command()` and `analyze_command()` stay for read mode and display. |

### src/tool/builtin/bash.rs

| Change | new | Add OS sandbox wrapper for child process spawning (future, separate task) |

## Tasks Affected

| Task                                 | Action                              |
| ------------------------------------ | ----------------------------------- |
| tk-w1ou (Persist tool approvals)     | Close — no approval system          |
| tk-mb8l (-w flag clear auto_approve) | Close — no -w flag, no auto_approve |
| tk-5h0j (Permission audit)           | Close — replaced by this design     |

## New Tasks

| Task                     | Priority | Detail                                                    |
| ------------------------ | -------- | --------------------------------------------------------- |
| Implement permissions v2 | P2       | Remove approval system, simplify modes, update CLI/config |
| OS sandbox for bash      | P3       | Landlock (Linux) + Seatbelt (macOS)                       |
| Config deny_commands     | P3       | Glob matching for blocked commands                        |
| MCP tool search          | P3       | Progressive disclosure for MCP tool descriptions          |
| Hooks engine             | P3       | Shell commands on lifecycle events                        |

## References

- Research: `ai/research/permission-systems-2026.md`
- Research: `ai/research/extensibility-systems-2026.md`
- Current design: `ai/design/permission-system.md` (v1, superseded)

## Status

**Phase**: Design complete, ready to implement
**Updated**: 2026-02-06
