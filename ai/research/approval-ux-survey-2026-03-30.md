# Approval UX Survey (2026-03-30)

How major coding agents handle tool approval and permission bypass.

## Summary

Three approaches dominate:

1. **Gate-based** (Claude Code, Codex): Sandbox + approval policy as orthogonal controls. Sandbox defines hard boundaries; approval policy decides when to prompt.
2. **Config-based** (OpenCode, Zed): Pure application-level permission rules. No OS sandbox. Simpler but less secure against prompt injection.
3. **No-gate** (Pi): No permissions. User responsible for isolation (Docker). Maximally flexible.
4. **Protocol-based** (ACP): Defines `requestPermission` RPC but delegates policy to agent/client pair.

## Tool-by-Tool Details

### Claude Code
- **6 modes**: default, acceptEdits, plan, auto, dontAsk, bypassPermissions
- **Bypass**: `--dangerously-skip-permissions` — skips ALL checks
- **Auto mode**: Classifier auto-approves/blocks (requires Team plan)
- **Per-tool rules**: `.claude/settings.json` with allow/deny + glob patterns
- **Approval UX**: `[y] Yes  [n] No  [a] Always allow  [d] Don't allow`
- **Read tools**: Never gated — always auto-approved
- **Sandbox**: OS-level, complementary to permissions
- **Key insight**: `[a] Always allow` persists per-session for edits, per-project for bash

### Codex CLI
- **Two orthogonal controls**: sandbox mode (`-s`) + approval policy (`-a`)
- **Convenience presets**: `--full-auto` = `-a on-request -s workspace-write`
- **Yolo**: `--dangerously-bypass-approvals-and-sandbox` (alias: `--yolo`)
- **Sandbox**: Platform-native (macOS Seatbelt, Linux bwrap), applies to spawned commands
- **Config**: `~/.codex/config.toml` with profiles
- **Key insight**: Sandbox and approval are separate axes. Can have sandbox without approval prompts.

### Gemini CLI
- **3 modes**: default, auto_edit, yolo
- **Yolo**: `--yolo` / `-y` / `--approval-mode yolo`
- **Policy Engine**: TOML rules with priority-based matching
- **auto_edit**: Auto-approve file edits, prompt for shell commands
- **Sandbox**: Docker-based, auto-enabled with --yolo
- **Permanent approval**: "Allow for all future sessions" auto-generates policy rule
- **Key insight**: auto_edit is the "daily driver" mode — edits auto, shell manual

### OpenCode
- **Config-based**: JSON per-tool rules with allow/ask/deny
- **Global toggle**: `"permission": "allow"` auto-approves everything
- **Very permissive defaults**: Most tools default to allow
- **Only ask defaults**: doom_loop, external_directory
- **No sandbox**: Pure application-level gates
- **Pattern matching**: Wildcard per tool key
- **Key insight**: Per-agent permission overrides in config

### Pi
- **No permission system at all** — explicit design choice
- **Isolation**: Run in Docker (user's responsibility)
- **Tool restriction**: `--tools read,grep,find,ls` to limit available tools
- **Extensions**: Can build custom permission gates via extension API
- **Key insight**: "Primitives, not features" philosophy

### Zed
- **Regex-based**: Rust regex per tool in settings.json
- **Global default**: `"default": "allow"` for auto-approve
- **Rule precedence**: Built-in security > always_deny > always_confirm > always_allow > tool default > global default
- **Built-in security**: Hardcoded deny for `rm -rf /`, `rm -rf ~`, etc.
- **MCP support**: `mcp:servername:toolname` keys
- **Approval UX**: Allow once / Always for tool / Always for input (extracts pattern)
- **Key insight**: Chained commands (`&&`) parsed, each sub-command checked independently

### ACP (Protocol)
- **`requestPermission` RPC**: Agent proposes options, client selects
- **Option kinds**: allow, deny, apply_patch, modify_request
- **Agent proposes options** — not client. Agent can offer alternatives.
- **Config options**: `session/set_config_option` for mode switching
- **No auto-approve definition**: Protocol is transport-agnostic about what gets auto-approved
- **Key insight**: Mode switching happens via permission request options (e.g., "Yes, and auto-accept all actions")

## Comparison Matrix

| Tool | Auto-approve Flag | Per-tool Rules | Sandbox | Deny Rules | Pattern Granularity |
|---|---|---|---|---|---|
| Claude Code | `--dangerously-skip-permissions` | settings.json allow/deny | OS-level | Yes (deny priority) | Gitignore (files), glob (bash) |
| Codex CLI | `--yolo` | config.toml rules | Platform-native | Yes | Command prefix |
| Gemini CLI | `--yolo` | Policy Engine TOML | Docker/Seatbelt | Yes | Command prefix |
| OpenCode | `"permission": "allow"` | JSON per tool | None | Yes | Wildcard |
| Pi | Always auto | `--tools` flag | None (use Docker) | No | N/A |
| Zed | `"default": "allow"` | Regex per tool | None | Yes | Full Rust regex |
| ACP | N/A (agent decides) | N/A | N/A | Via options | N/A |

## Design Implications for Ion

1. **`/yolo` naming**: Gemini and Codex both use `--yolo` as an alias. Claude Code uses `--dangerously-skip-permissions`. The naming is converging on `yolo`.
2. **auto_edit is a popular middle ground**: Both Claude Code (acceptEdits) and Gemini (auto_edit) have a mode that auto-approves file edits but prompts for shell.
3. **"Always allow" at approval time is key UX**: Claude Code, Zed, and Gemini all offer to persist the approval decision. This is the biggest UX win beyond /yolo.
4. **Sandbox is separate from approval**: Codex and Gemini both treat sandbox as orthogonal. Ion's `tk-8s0h` covers sandbox separately.
5. **Read tools are never gated**: Universal across all agents.
