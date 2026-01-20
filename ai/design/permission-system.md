# Permission System Design

**Date**: 2026-01-18
**Status**: Approved
**Task**: tk-a8vn

## Overview

ion uses a sandboxed permission model with three modes and composable CLI flags.

## Modes

| Mode      | Description                                             |
| --------- | ------------------------------------------------------- |
| **Read**  | Read-only tools (read, glob, grep)                      |
| **Write** | Write/edit auto-allowed, bash needs per-command approval |
| **AGI**   | Full autonomy, no prompts                               |

## CLI Flags

| Flag           | Short | Purpose                           |
| -------------- | ----- | --------------------------------- |
| `--read`       | `-r`  | Read-only mode                    |
| `--write`      | `-w`  | Write mode (explicit)             |
| `--yes`        | `-y`  | Auto-approve (implies -w)         |
| `--no-sandbox` | -     | Allow outside CWD                 |
| `--agi`        | -     | Full autonomy (= -y --no-sandbox) |

## Permission Matrix

| Tool  | Read    | Write              | Write + -y | AGI  |
| ----- | ------- | ------------------ | ---------- | ---- |
| read  | auto    | auto               | auto       | auto |
| glob  | auto    | auto               | auto       | auto |
| grep  | auto    | auto               | auto       | auto |
| write | blocked | auto               | auto       | auto |
| edit  | blocked | auto               | auto       | auto |
| bash  | blocked | approval (per-cmd) | auto       | auto |

## CWD Sandbox

By default, all operations are restricted to the current working directory.

| Flag           | CWD Restricted |
| -------------- | -------------- |
| (none)         | Yes            |
| `--yes`        | Yes            |
| `--no-sandbox` | No             |
| `--agi`        | No             |

## Examples

```bash
# Default: write mode, CWD only (bash requires approval)
ion

# Read-only mode
ion -r

# Write mode (explicit)
ion -w

# Auto-approve in CWD
ion -y

# Approval required, but can access outside CWD
ion --no-sandbox

# Full autonomy (auto-approve + no sandbox)
ion --agi
ion -y --no-sandbox  # equivalent

# Read anywhere (no writes)
ion -r --no-sandbox

# Combinations
ion -wy              # same as -y (redundant but valid)
ion -ry              # warning: -y ignored in read mode
```

## TUI Behavior

- Default: Read ↔ Write (Shift+Tab to cycle)
- With `--agi`: Read ↔ Write ↔ AGI

AGI mode is only available in TUI if started with `--agi` or `-y --no-sandbox`.

## Approval Options

When prompted for approval:

| Key | write/edit               | bash                        |
| --- | ------------------------ | --------------------------- |
| `y` | approve once             | approve once                |
| `n` | deny                     | deny                        |
| `a` | allow for session (tool) | allow for session (command) |
| `A` | allow permanently (tool) | allow permanently (command) |

Bash approval is per-command, not per-tool.

## Config

```toml
# ~/.ion/config.toml

[permissions]
default_mode = "write"      # read, write, agi
auto_approve = false        # --yes behavior
allow_outside_cwd = false   # --no-sandbox behavior
```

CLI flags override config.

## Implementation Tasks

- [x] Add -r/--read, -w/--write flags to CLI
- [x] Add -y/--yes flag (update existing)
- [x] Add --no-sandbox flag
- [x] Add --agi flag
- [x] Implement CWD boundary checking for tools
- [x] Add per-command bash approval storage
- [x] Update TUI mode cycling (AGI only with flag)
- [x] Add config support for permissions
- [x] Add warnings for useless flag combinations (-r -y)

## Implementation Notes

**Completed 2026-01-19**

Files modified:

- `src/cli.rs` - Global CLI flags, PermissionSettings struct
- `src/config/mod.rs` - PermissionConfig for config file support
- `src/main.rs` - Pass config to resolve_permissions
- `src/tui/mod.rs` - TUI mode cycling respects agi_enabled
- `src/tool/types.rs` - ToolContext.check_sandbox() method
- `src/tool/permissions.rs` - Per-command bash approval
- `src/tool/mod.rs` - Bash command-specific permission checking
- `src/session/mod.rs` - no_sandbox field on Session
- `src/agent/mod.rs` - Pass no_sandbox to ToolContext
- `src/tool/builtin/read.rs`, `write.rs`, `grep.rs` - Use ctx.check_sandbox()
