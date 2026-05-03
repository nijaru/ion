# Tools and Modes

## Default model-visible tool surface

| Tool | Category | Purpose |
|---|---|---|
| `read` | Read | Read file contents with model-visible line numbers |
| `grep` | Read | Search file contents |
| `glob` | Read | Find files by pattern |
| `list` | Read | List directory contents |
| `edit` | Write | Edit file with exact old/new string replacement and optional `expected_replacements` |
| `write` | Write | Write entire file |
| `multi_edit` | Write | Apply multiple exact replacements atomically |
| `bash` | Execute | Run shell command (streaming) |

This is the native-loop baseline. Keep it small unless an eval-backed tool
change clearly improves daily coding reliability.

Durable tool-surface decisions:

- `write` stays separate from targeted edits. Create/overwrite has a different
  risk and display shape than exact replacement.
- `edit` and `multi_edit` stay separate for now. A Pi-style merged
  `edit(edits[])` surface remains the best simplification candidate, but it
  needs a small local edit eval before replacing working tools.
- Structured edits are the normal edit path. Do not steer models toward Python,
  `sed`, heredocs, or shell patching for routine edits.
- Keep `grep`, `glob`, and `list` as typed read-only tools instead of collapsing
  ordinary discovery into `bash`.
- `bash` remains the escape hatch for repo-specific commands, verification, and
  advanced tools such as `rg`, `fd`, or `ast-grep` when the typed tools are not
  enough.

### Canto coding primitive adoption

Current decision after `tk-t818`: keep Ion's model-visible tool wrappers
Ion-owned for now, while continuing to use Canto's lower-level framework
contracts for agent loop, tool lifecycle, approvals, and provider history.

| Area | Canto primitive | Ion decision |
|---|---|---|
| `read` | `coding.ReadFileTool` | Keep Ion wrapper: tool name is shorter, output has stable line numbers, offset/limit, BOM/CRLF display cleanup, and Ion display compaction. |
| `write` | `coding.WriteFileTool` | Keep Ion wrapper: Ion needs checkpoints, TUI diff/display policy, and product-specific success output. |
| `edit` / `multi_edit` | `coding.EditTool`, `coding.MultiEditTool` | Keep Ion wrapper: Ion has `old_string`/`new_string`, CRLF/BOM matching, `replace_all`, `expected_replacements`, line-numbered ambiguity/count errors, checkpoints, and unified diff output. |
| `list` | `coding.ListDirTool` | Keep Ion wrapper: model-facing name/output are already tuned and mode-filtered with the rest of the read tool set. |
| `grep` / `glob` | no stable Canto primitive | Keep Ion-owned ripgrep-backed tools; revisit ripgo only after benchmark/eval evidence. |
| `bash` | `coding.ShellTool` / `coding.Executor` | Keep Ion wrapper: Ion owns shell name, sandbox posture, process-group cancellation, streaming deltas, output truncation markers, and future background-job UX. |
| `execute_code` | `coding.CodeExecutionTool` | Do not add by default. Python/code execution is not a core coding-agent edit path and should stay behind explicit future eval/safety work. |

Adoption rule: use Canto coding primitives when the primitive is a substrate
that preserves Ion's model-facing schema, policy/display hooks, checkpoints,
and test coverage. Do not replace Ion's wrappers merely to reduce local code.

### Edit surface design

Current recommendation after the post-I2 evaluation:

- Keep `write` separate long-term. Whole-file create/overwrite is a different
  operation from targeted replacement for approval, display, and recovery.
- Keep `edit` and `multi_edit` as the current I4 surface. Ion's implementation
  is already hardened around exact replacement, CRLF/BOM-safe matching,
  line-numbered ambiguity/count errors, explicit replacement counts, and
  atomic validation before writes.
- Do not make Python, `sed`, heredocs, or shell patching the normal edit path.
  They are harder for agents to quote correctly, bypass Ion's edit-specific
  validation/diff display, weaken permission/audit boundaries, and are easier
  to partially apply.
- Do not adopt a Codex-style patch grammar as the default Ion edit surface yet.
  It is powerful and well-specified, but it adds a second model-facing language
  and depends on grammar/freeform support quality across providers.

Reference synthesis:

| Reference | Edit shape | Takeaway for Ion |
|---|---|---|
| Pi | `write` plus one structured `edit(path, edits[])` | Good target for fewer tools and multi-edit preview UX |
| Claude-like tools | separate file read/write/edit plus grep/glob/bash classes | Confirms clear permission/display classes are conventional |
| Codex | `apply_patch` grammar for add/update/move/delete | Useful later for power users; too heavy for default mixed-provider path |

Deferred implementation:

- Implement a merged tool only after a small local eval shows it is equal or
  better than the current split. Ion is `v0.0.0`, so no compatibility shim is
  needed if the eval says merge.
- Preferred candidate:

```json
{
  "edits": [
    {
      "file_path": "path/to/file.go",
      "old_string": "exact unique text",
      "new_string": "replacement text",
      "replace_all": false,
      "expected_replacements": 1
    }
  ]
}
```

Acceptance criteria before merging:

- Equal or better edit success on a small local eval set covering single edit,
  many edits in one file, multi-file edits, duplicate matches, missing matches,
  overlapping edits, CRLF, BOM, and large files.
- No partial writes on validation failure or cancellation.
- Error messages point to the failing edit index, path, replacement count, and
  line numbers where useful.
- TUI displays `Edit(path)` with compact success by default and diff expansion
  on demand.
- Provider compatibility is verified against at least local-api, OpenRouter, and
  one OpenAI/Anthropic-family model before making it default.

Deferred or hidden surfaces:

| Surface | Status |
|---|---|
| `recall_memory`, `remember_memory` | Deferred until memory is deliberately reopened |
| model-visible `compact` tool | Removed; `/compact` host command remains available for context survival |
| MCP tools | Deferred behind the native-loop stabilization gate |
| `subagent` | Opt-in I4 surface via `subagent_tools = "on"`; not default |
| `ask_user` | Deferred until Canto owns a general elicitation/pause-resume primitive |
| `verify` | Removed; normal verification goes through `bash` |

## Virtual Resource Namespaces

Virtual namespaces are a future capability boundary, not a default tool-surface
expansion. They let Ion route non-workspace resources through one policy and
display layer without pretending they are ordinary repo files.

Initial namespace model:

| Namespace | Example URI | Capability | Default exposure |
|---|---|---|---|
| workspace | `workspace://internal/app/model.go` | read/search/write/execute through the existing eight tools | default |
| skill | `skill://review/SKILL.md` | read installed skill bodies and metadata | host `/skills`; opt-in `read_skill` |
| memory | `memory://project/<id>` | search/read/write durable memories | deferred |
| artifact | `artifact://session/<id>/<name>` | read compaction/offload artifacts | deferred |

Rules:

- Do not add per-namespace tools such as `read_memory`, `search_memory`,
  `list_skills`, and `read_artifact` by default.
- Keep workspace file tools scoped to the workspace filesystem. Do not silently
  overload `read(path)` so `memory://...` or `skill://...` behaves like a real
  file; that would blur permissions and confuse edit semantics.
- Namespace resolvers should be host/framework capabilities with explicit
  policy metadata: readable, searchable, writable, executable, audited,
  prompt-cost-bearing, and display-safe.
- If multiple non-workspace namespaces become model-visible, prefer a small
  opt-in resource surface over many bespoke tools:

```json
read_resource({"uri": "skill://review/SKILL.md"})
search_resource({"namespace": "memory", "query": "provider history bug"})
```

- Do not implement `write_resource` until memory/skill mutation has a clear
  approval, audit, and undo contract. Until then, mutation stays host-owned
  through commands such as `ion skill install --confirm`.
- Progressive disclosure is mandatory: enabling a namespace may expose a
  narrow tool and short guidance for that namespace, but it must not inject a
  full inventory into the default prompt.

Ownership:

- Canto may own a generic resource namespace interface if multiple hosts need
  it: resolver registration, URI parsing, policy metadata, and durable
  references.
- Ion owns product choices: which namespaces are mounted, which tools become
  model-visible, how `/skills` or future `/memory` surfaces display resources,
  and when mutation requires approval.

Supporting infrastructure:

- `ApprovalManager` — goroutine-safe request/response channels
- `PolicyEngine` — category-to-policy mapping with mode-awareness
- Model-visible tool results are size-bounded with explicit truncation markers;
  TUI display compaction is separate and must not be the only place truncation is visible.
- Prompt/tool budget is measured separately in
  `ai/research/prompt-budget-2026-05.md`; do not add model-visible tools without
  re-running the budget report.
- Open-ended user elicitation should not be an Ion-only model-visible tool.
  Models can ask normal assistant questions today. A future `ask_user` surface
  needs a Canto-level interaction primitive that can pause/resume safely and
  fail clearly in noninteractive hosts.

## Permission Modes And Trust

## Background Bash Monitor

Background bash is an I4 table-stakes workflow for dev servers, file watchers,
and long-running test loops. The design target is useful Claude-like behavior
without growing the default model-visible tool count.

Direction:

- Keep one model-visible `bash` tool. Do not add separate default
  `bash_output`, `bash_kill`, or `monitor` tools unless evals show models fail
  with the unified shape.
- Extend `bash` with an explicit action shape:
  - `run` starts a command; `background: true` returns a live job id instead of
    blocking for command completion.
  - `output` reads buffered stdout/stderr for a job id with optional tail limits.
  - `kill` terminates a job id and its process group.
- Keep ordinary foreground `bash` simple and compatible with the current command
  field. Background mode should be opt-in and obvious in the tool description.
- Job state is live session state. Transcript rows record job ids and retrieved
  output, but Ion does not promise that background processes survive app exit or
  restart in the first implementation.
- Background process execution uses the same workspace, policy category, and
  sandbox posture as foreground `bash`.
- `Session.Close()` must kill any remaining background process groups.
- TUI display stays compact by default:
  - `Bash(npm run dev) · background job bash-1`
  - `Bash(output bash-1) · 42 lines`
  - `Bash(kill bash-1)`

Implementation should stay Ion-owned first. Canto only needs a new framework
primitive if multiple tools need durable async task handles or if tool progress
needs to outlive the hosting process.

Acceptance criteria before implementation is considered done:

- starting, polling, and killing a background command are covered by tool tests
- cancellation of a foreground turn does not orphan background jobs
- closing the session kills remaining jobs
- sandbox failures fail closed before job registration
- TUI/replay rows use the shared tool display formatter and do not dump routine
  output by default

### Relationship to `/goal`

Long-running task goals need background/job substrate first. Codex's `/goal`
shape is useful as a later reference, but Ion should not add a goal command
until the product can track objective status, pause/resume, token/time usage,
and recovery across resume. The first command layer for this area is background
visibility (`/tasks` or `/ps`) and cancellation (`/stop`), not model-visible
goal text.

### Design principles

- Modes are **permission postures**, not workspace trust decisions
- Trust answers whether a workspace may leave read-only safety
- Mode answers how much approval Ion needs once the workspace is trusted
- Read tools are never gated in any mode
- Sandbox is orthogonal to permissions. It is an executor capability, not a
  mode.

### Mode table

| Category | READ | EDIT | AUTO |
|---|---|---|---|
| Read | auto | auto | auto |
| Write | **blocked** | prompt | auto |
| Execute (`bash`) | **blocked** | prompt | auto |
| Sensitive non-read tools | hidden or blocked | prompt | auto |

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

MCP is still deferred. `subagent` is available only through the explicit
`subagent_tools = "on"` config gate. It remains a sensitive surface: hidden in
READ mode, prompted in EDIT mode, and auto-approved only in AUTO.

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
subagent_tools = "off"            # off | on; off by default
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

### Executor and credential boundary

The long-term shape is one tool executor boundary:

```text
tool request -> policy/mode/trust -> executor -> sandbox -> process/remote job
```

Rules:

- Approval mode decides whether Ion asks before a tool runs.
- Workspace trust decides whether this checkout may leave READ mode.
- Sandbox decides what an approved process can touch.
- The executor owns process start, cancellation, process-group cleanup,
  stdout/stderr streaming, output limits, background jobs, and future remote
  execution.
- Provider credentials are not tool credentials. API keys used to call models
  must not be injected into tool subprocesses by default.
- Tool credentials must come through an explicit secret-injection boundary with
  display/log redaction and an auditable scope.
- Remote sandboxes, containers, or `just_bash`-style executors should plug in as
  executor implementations. They must not add another agent loop, another
  provider-history owner, or another transcript writer.

Current implementation note:

- Ion's local `bash` tool now delegates process planning/execution to a local
  executor object. The model-facing `bash` schema remains unchanged.
- The executor preserves the current subprocess environment behavior for now:
  local bash inherits Ion's process environment. Do not change this implicitly
  during cleanup, because common developer commands depend on inherited
  toolchain, shell, SSH agent, and cloud-profile variables.
- Environment policy is a staged hardening surface:
  - current default: `inherit`
  - implemented explicit mode: `inherit_without_provider_keys`
  - future explicit modes: `minimal` and `allowlist`
- Provider credentials are not tool credentials. When
  `tool_env = "inherit_without_provider_keys"`, provider API-key variables from
  the provider catalog are denied to subprocesses while the rest of the
  inherited developer environment is preserved.
- Tool secrets require an explicit named-secret injection path with user
  approval, display/log redaction, and audit records that include names and
  scopes but never values.
- Do not add a model-visible secret field to `bash` until the explicit
  injection and redaction path exists. Until then, sandbox posture must stay
  visible and AUTO must not imply isolation when `Sandbox: off`.

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
- Sandbox is orthogonal to approval and belongs at the executor boundary.
- Read tools are never gated in any agent

These findings are distilled here; the older survey note has been removed from
active `ai/` context.

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

`grep`, `glob`, and `list` remain dedicated read-only tools during P1. They
preserve clearer policy, transcript display, truncation, path containment, and
replay behavior than raw shell commands.

Current baseline is ripgrep semantics. Ripgo remains a deferred benchmarked
replacement candidate; it should only replace the current behavior after
matching ignored-file, hidden-file, `.git` exclusion, cancellation, truncation,
path-containment, and large-repo latency expectations.

Tracked by: `tk-8fe3`, `tk-03hf`

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
