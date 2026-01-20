# ion Tool Permissions & Execution Modes

**Status**: Finalized (Simple & Sharp)
**Date**: 2026-01-14

## 1. Execution Modes

Three simple modes to control the agent's autonomy.

| Mode      | Behavior                                                                    | Use Case                     |
| :-------- | :-------------------------------------------------------------------------- | :--------------------------- |
| **Read**  | `read`, `glob`, `ls`, `grep` are auto-approved. Mutations are **blocked**.  | Safe exploration.            |
| **Write** | (Default) `READ` tools are auto-approved. `write` and `bash` require `y/n`. | Standard interactive coding. |
| **AGI**   | All tools are auto-approved. No prompts.                                    | Full autonomy (Bypass mode). |

## 2. The Approval Prompt

When a tool requires permission (in **Write** mode), the TUI presents:

- **`y`**: Yes (once)
- **`n`**: No (reject)
- **`a`**: Always for this session
- **`A`**: Always permanent (updates `config.toml`)
- **`s`**: Toggle Summary/Details (MVP: List of files vs Full Diff)

## 3. Tool Classification

| Level          | Tools                               | Behavior (in Write Mode) |
| :------------- | :---------------------------------- | :----------------------- |
| **Safe**       | `read`, `glob`, `ls`, `grep`        | Auto-approve             |
| **Restricted** | `write`, `bash`, `delete`, `create` | Prompt `y/n/a/A/s`       |

## 4. Configuration

```toml
[permissions]
# Permanently allow these tools
allow_always = ["write"]
```
