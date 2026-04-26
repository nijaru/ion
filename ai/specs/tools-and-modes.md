# Tools and Modes

## Built-in tool surface

| Tool | Category | Purpose |
|---|---|---|
| `read` | Read | Read file contents |
| `grep` | Read | Search file contents |
| `glob` | Read | Find files by pattern |
| `list` | Read | List directory contents |
| `recall_memory` | Read | Recall from session memory |
| `remember_memory` | Read | Store in session memory |
| `compact` | Read | Trigger context compaction |
| `edit` | Write | Edit file with old/new string replacement |
| `write` | Write | Write entire file |
| `multi_edit` | Write | Apply multiple edits atomically |
| `bash` | Execute | Run shell command (streaming) |
| `verify` | Execute | Run test/benchmark and report results |
| `mcp` | Sensitive | Call MCP server tool |
| `subagent` | Sensitive | Spawn child agent |

Supporting infrastructure:

- `ApprovalManager` — goroutine-safe request/response channels
- `PolicyEngine` — category-to-policy mapping with mode-awareness

## Permission Modes

### Design principles

- Modes are **permission boundaries**, not behavioral hints
- One semantic jump between each mode
- The approval prompt is the granularity mechanism — modes are coarse
- Read tools are never gated in any mode
- Sandbox is orthogonal to permissions (tracked: `tk-kfno`)

### Mode table

| Category | READ | EDIT | YOLO |
|---|---|---|---|
| Read | auto | auto | auto |
| Write | **blocked** | prompt | auto |
| Execute (bash) | **blocked** | prompt | auto |
| Sensitive (mcp, subagent) | prompt | prompt | auto |

### Mode cycling

`Shift+Tab` cycles: READ → EDIT → YOLO → READ

`/yolo` toggles YOLO on/off (returns to EDIT when toggling off)

`/mode [read|edit|yolo]` for explicit set

Default startup: EDIT

### READ

Look-only. No mutations, no execution.

Bash is **entirely blocked** — not safe-listed, not prompted. The read
tools (read, grep, glob, list) already cover everything the agent needs
to understand a codebase. Bash in READ mode would be an escape hatch
that undermines the mode's guarantee.

MCP/subagent is **prompted** — these calls may be read-only (fetching a
doc) so the user decides.

Status line: `[READ]` (cyan)

### EDIT (default startup mode)

Normal work. The agent can do anything with approval.

File edits, bash, and MCP all prompt. This is the daily-driver mode —
the agent can work on code but you see and approve each action.

Status line: `[EDIT]` (green)

### YOLO

Full auto. Every tool auto-approved. No prompts.

Activated via `/yolo` toggle or `/mode yolo`. Use when you trust the
agent's direction and want speed over visibility.

Status line: `[YOLO]` (red)

### Why not a plan mode?

READ mode already provides "look, don't touch." Plan mode is really a
system prompt instruction ("analyze first, ask permission to execute"),
not a permission boundary. Can add a `/plan` command later that sets
READ + injects a planning instruction. Not a mode.

## Approval prompt

When a tool needs approval (EDIT mode only — READ blocks, YOLO
auto-approves):

```
  ⚠ bash: npm install --save react
  [y] Yes  [n] No  [a] Always this session
```

- **`y`** — approve once
- **`n`** — deny
- **`a`** — auto-approve all remaining calls for this tool category,
  this session only. Sets category policy to `PolicyAllow` in-memory.
  No config file. No persistence. Resets on next session.

The `a` key handles 90% of approval fatigue without needing YOLO mode.
It's the "I trust edits now, stop asking" escape hatch.

Future: `A` (shift+a) could persist category approvals across sessions
via config. Requires config schema, UI to manage rules, security
considerations. Ship later.

## Config

```toml
default_mode = "edit"             # read | edit | yolo
policy_path = "~/.ion/policy.yaml" # optional; default path when unset
```

User-global only — project configs cannot weaken permissions (same security
model as the Rust era). `policy_path` points to YAML rules for exact tools or
categories; see `ai/specs/security-policy.md` and `docs/security/policy.md`.

## CLI Flags

- `--mode read|edit|yolo` — start in the selected permission mode
- `--yolo` — start in YOLO mode (alias for `--mode yolo`)

## Sandbox

Bash sandboxing is configured outside the approval modes:

```text
ION_SANDBOX=off|auto|seatbelt|bubblewrap
```

Current behavior:

- `off`: plain bash
- `auto`: use macOS Seatbelt or Linux bubblewrap when available; otherwise
  visibly report fallback to `off`
- `seatbelt`: require `sandbox-exec`; fail closed if unavailable
- `bubblewrap`: require `bwrap`; fail closed if unavailable

`/tools` reports the active sandbox posture for the native backend.

## Escalation

Ion loads `ESCALATE.md` from the workspace root when present, using Canto's
root-scoped parser. The current host behavior is deliberately narrow:
approval prompts surface declared email/Slack channels and approval timeout
metadata so a blocked local run has an explicit handoff path. Automated
Slack/email delivery is a separate notifier layer and should not be added
until credentials, delivery semantics, and audit logging are designed.

## Research

Surveyed Claude Code, Codex CLI, Gemini CLI, OpenCode, Pi, Zed, ACP.
Key findings:

- `yolo` naming converges across Codex and Gemini
- "auto_edit" middle ground (auto edits, prompt bash) is industry
  consensus for daily driving — but we prompt for edits too in EDIT mode
- "Always allow" at approval time is the biggest UX win beyond modes
- Sandbox is orthogonal to approval (tracked separately: `tk-kfno`)
- Read tools are never gated in any agent

Full details: `ai/research/approval-ux-survey-2026-03-30.md`

## Rust reference

The Rust archive had a safe-command whitelist for READ mode. The Go
implementation goes further: READ mode blocks bash entirely rather than
maintaining a safe-command whitelist. The safe-command whitelist
(`bash_policy.go`) still exists but is used only for informational
display in the policy layer.

Reference: `archive/rust/src/tool/builtin/guard.rs`

Tracked by: `tk-k4hv`

## Important files

- `internal/backend/policy.go` — PolicyEngine, Authorize()
- `internal/backend/bash_policy.go` — IsSafeBashCommand()
- `internal/backend/canto/backend.go` — policyHook (pre-tool-use hook)
- `internal/backend/canto/tools/approver.go` — ApprovalManager
- `internal/session/types.go` — Mode enum
- `internal/session/event.go` — ApprovalRequest event
- `internal/session/session.go` — AgentSession interface (Approve, SetMode)
- `internal/app/events.go` — approval gate (y/n/a), session event handling
- `internal/app/render.go` — approval prompt rendering
- `internal/app/commands.go` — slash command dispatch
- `internal/app/model.go` — Mode field, Shift+Tab handler

## Other tool topics

### Tool presentation

Current Go transcript rendering shows tool calls too literally (raw JSON
args, too much structure). Direction: compact header, extracted key arg,
bounded output preview.

Tracked by: `tk-h4bp`, `tk-8fe3`

### Search tools

`grep` prefers external `rg` with Go-native fallback. `glob` is built
in. Open question: should ion steer models toward built-in search
tools? Should ion adopt a stronger native grep?

Tracked by: `tk-8fe3`, `tk-yp24`

### Retry behavior

Transient provider failures are retried automatically with exponential
backoff before surfacing to the UI. Ion keeps the retry loop silent unless
all attempts fail, in which case only the final error is shown once in the
status surface and transcript.

Tracked by: `tk-kz3k`
