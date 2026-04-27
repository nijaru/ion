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

## Permission Modes And Trust

### Design principles

- Modes are **permission postures**, not workspace trust decisions
- Trust answers whether a workspace may leave read-only safety
- Mode answers how much approval Ion needs once the workspace is trusted
- Read tools are never gated in any mode
- Sandbox is orthogonal to permissions (tracked: `tk-kfno`)

### Mode table

| Category | READ | EDIT | AUTO |
|---|---|---|---|
| Read | auto | auto | auto |
| Write | **blocked** | prompt | auto |
| Execute (bash) | **blocked** | prompt | auto |
| Sensitive (mcp, subagent) | prompt | prompt | auto |

`yolo` remains a command/CLI alias for `auto`; it should not be the displayed
mode name.

### Trust gate

Trust is user-global workspace eligibility, stored outside project files.

| Workspace trust | Startup behavior | Mode availability |
|---|---|---|
| trusted | use requested/configured mode | `read`, `edit`, `auto` |
| untrusted | force `read` and show a startup notice | `read` only until `/trust` |

`/trust` means: allow normal edit/auto behavior in this workspace. It does not
auto-approve tools, disable sandboxing, or trust project instructions blindly.

Untrusted workspaces should block attempts to enter `edit` or `auto` with copy
like: `Trust this workspace first with /trust.`

Config should support a trust policy:

```toml
workspace_trust = "prompt" # prompt | off | strict
```

- `prompt`: default; unknown workspaces start in `read`, `/trust` enables normal modes
- `off`: no trust gate; start in requested/configured mode everywhere
- `strict`: enterprise posture; unknown workspaces stay `read`, `/trust` may be disabled or admin-managed

### Mode cycling

`Shift+Tab` toggles only `READ <-> EDIT`.

If currently in `AUTO`, `Shift+Tab` drops to `EDIT`. It must never enter
`AUTO`, because accidental key cycling should not grant unattended execution.

`AUTO` requires an explicit command or startup flag:

- `/auto`
- `/yolo` alias
- `/mode auto`
- `ion --mode auto`
- `ion --yolo`

`/mode [read|edit|auto]` is the canonical command shape.

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

### AUTO

Full auto. Every tool auto-approved. No prompts.

Activated via `/auto`, `/yolo`, or `/mode auto`. Use when you trust the
agent's direction and want speed over visibility.

Status line: `[AUTO]` (red)

When enabling AUTO, print a short host notice that includes sandbox posture:
`AUTO mode enabled. Writes and commands run without approval. Sandbox: <state>.`

### Why not a plan mode?

READ mode already provides "look, don't touch." Plan mode is really a
system prompt instruction ("analyze first, ask permission to execute"),
not a permission boundary. Can add a `/plan` command later that sets
READ + injects a planning instruction. Not a mode.

## Approval prompt

When a tool needs approval (EDIT mode only — READ blocks, AUTO
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

The `a` key handles 90% of approval fatigue without needing AUTO mode.
It's the "I trust edits now, stop asking" escape hatch.

Future: `A` (shift+a) could persist category approvals across sessions
via config. Requires config schema, UI to manage rules, security
considerations. Ship later.

## Config

```toml
default_mode = "edit"             # read | edit | auto
workspace_trust = "prompt"        # prompt | off | strict
policy_path = "~/.ion/policy.yaml" # optional; default path when unset
```

User-global only — project configs cannot weaken permissions (same security
model as the Rust era). `policy_path` points to YAML rules for exact tools or
categories; see `ai/specs/security-policy.md` and `docs/security/policy.md`.

## CLI Flags

- `--mode read|edit|auto` — start in the selected permission mode
- `--yolo` — start in AUTO mode (alias for `--mode auto`)

If workspace trust is `prompt` or `strict`, an untrusted workspace still starts
in `read` unless policy explicitly disables the trust gate.

## Slash Commands During Active Turns

Host-only slash commands should remain available while the agent is in a turn.
This includes mode, model, provider, settings, cost, tools, and trust status
commands. Commands that mutate the active backend/model should either apply to
the next turn or clearly report that the current turn is still using the prior
runtime.

Minimum command set:

- `/mode read|edit|auto`
- `/read`, `/edit`, `/auto`, `/yolo`
- `/model`, `/provider`, `/settings`
- `/cost`, `/tools`
- `/trust`, `/trust status`

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

The startup banner and `/tools` report the active sandbox posture for the native
backend.

## Escalation

Ion loads `ESCALATE.md` from the workspace root when present, using Canto's
root-scoped parser. Approval prompts surface declared email/Slack channels and
approval timeout metadata so a blocked local run has an explicit handoff path.

First notifier delivery slice:

- Slack channels send through an incoming webhook URL read from the channel's
  `webhook_env` metadata value, or `ION_SLACK_WEBHOOK_URL` when unset.
- Email channels send through SMTP only when `ION_SMTP_ADDR` and
  `ION_SMTP_FROM` are set; optional auth uses `ION_SMTP_USERNAME` and
  `ION_SMTP_PASSWORD`. Channel metadata can override env names with
  `smtp_addr_env`, `from_env`, `smtp_user_env`, and `smtp_pass_env`.
- Missing credentials are audited as `skipped`, not surfaced as user-facing
  approval errors.
- Delivery failures are audited as `failed` and printed as system notices; they
  must not block the local approval prompt.
- Every attempted channel writes an `escalation_notification` audit record with
  request id, channel, target, status, detail, and timestamp.

## Research

Surveyed Claude Code, Codex CLI, Gemini CLI, OpenCode, Pi, Zed, ACP.
Key findings:

- `yolo` naming converges across Codex and Gemini, but Ion displays AUTO
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

Transient provider/network failures retry automatically with exponential
backoff. By default `retry_until_cancelled = true`, so retryable network,
rate-limit, and provider-capacity failures continue until the user interrupts.
Ion surfaces the retry in the progress/status line and persists the status
event; it does not spam transcript history with each attempt.

When `retry_until_cancelled = false`, Ion uses Canto's bounded internal retry
budget before surfacing the final error once in the status surface and
transcript.

Terminal provider errors are not retried: auth failures, invalid models, bad
endpoint config, quota/billing exhaustion, and context-limit failures.

Tracked by: `tk-kz3k`, `tk-lm25`
