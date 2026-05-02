# ion Design

Updated: 2026-05-02

## Product Shape

Ion is a standalone terminal coding agent in the same category as Pi, Claude
Code, and Codex. The core product is a fast, reliable solo agent with a minimal
TUI and scriptable CLI. Advanced capabilities are built around that core, not
inside it.

Reference posture:

- Pi is the reliability and simplicity floor.
- Claude Code and Codex are UX, CLI, session, and tool-quality references.
- Canto is the framework layer; Ion is the product layer.

## Layering

| Layer | Owner | Responsibility |
| --- | --- | --- |
| Product | Ion | TUI, CLI, commands, settings, tool UX, workspace policy, provider choices |
| Framework | Canto | Agent loop, durable events, provider-visible history, compaction primitives, tool lifecycle |
| Provider | Canto `llm` | Provider requests, streaming, token/cost accounting, model capabilities |
| Transport | Provider clients | HTTP/SSE/JSON-RPC details |

Native mode is primary:

```text
ion TUI/CLI -> CantoBackend -> canto -> provider API
```

ACP and subscription bridges are secondary compatibility paths. They must not
drive Ion's native architecture.

Ion can also run as an ACP agent:

```text
external ACP host -> ion --agent -> AgentSession -> CantoBackend -> canto -> provider API
```

The headless ACP surface is a host-integration adapter around Ion's existing
runtime boundary. It must translate prompts, stream updates, tool calls,
approval requests, cancellation, and modes without creating another agent loop
or another transcript writer.

## Harness Direction

Flue, Pi, OpenAI Agents SDK, and the Mendral sandbox split all point at the
same shape: the harness is the deterministic runtime around the model, and the
TUI is only one host for that runtime. Ion should not copy Flue's TypeScript or
deployment model, and it should not copy Mendral's multi-user backend before
the local agent is stable. These references are applicable only where they
simplify Ion's core boundary:

```text
host shell -> agent runtime -> session -> tools/sandbox/env -> durable events
```

For Ion this means:

- The default local coding loop remains the product baseline. Any harness
  refactor must make that path smaller and easier to test, not introduce a new
  feature layer.
- `CantoBackend` should become a thin product adapter over a clearer Canto
  harness facade, not a second framework hidden inside Ion.
- Ion's TUI/CLI should consume runtime events and send host decisions; it
  should not own provider-visible history, tool lifecycle semantics, child
  session ownership, or compaction correctness.
- Tool execution should be expressed as capabilities over a session
  environment: shell, read, write, edit, list, search, custom commands, and
  future remote sandboxes.
- Tool, skill, memory, and sandbox expansion must preserve a small
  model-facing surface. If future memory/skills need file-like access, prefer
  backend path routing or explicit progressive-disclosure tools over adding
  many near-duplicate tools.
- Approval, future elicitation, cancellation, and active-turn steering should
  converge on one typed interrupt/pause-resume boundary so TUI, CLI, ACP, and
  headless hosts stay consistent.

What does not follow:

- Do not add a remote sandbox, virtual filesystem, memory namespace, or
  multi-agent runtime as part of the minimal-core pass.
- Do not expose more default tools because another framework supports them.
- Do not reshape Ion around a generic hosted-agent platform; Ion is first a
  local terminal coding agent.

## Core Runtime Contract

Ion has one native baseline path. There is no global stabilization branch.

The baseline contract is:

```text
submit -> stream -> tool call/result -> terminal event -> persist -> replay -> follow-up
```

Canto owns:

- provider-visible message history and effective-history projection
- agent turn execution and tool lifecycle events
- retry, cancellation settlement, queueing, and terminal turn states
- compaction primitives and context-overflow recovery

Ion owns:

- user input classification and slash/local command dispatch
- TUI/CLI lifecycle and runtime provider/model selection
- transcript display projection and compact/full tool rendering
- local status rows, progress shell, queued input, and footer state
- workspace trust, modes, and policy UX

Ion must not create a second provider-visible transcript writer. Storage and
rendering may add display rows, but Canto-derived durable history is the source
for model requests.

The current refactor target is a minimal core, not a broader harness platform:

```text
input -> runtime session -> stream/tool events -> durable log -> replay/follow-up
```

Everything outside that line is opt-in or deferred until the line is boring
under deterministic, race, tmux, and live-provider gates.

## Tool Surface

The P1 native tool surface is intentionally small:

```text
bash, read, write, edit, multi_edit, list, grep, glob
```

Rules:

- Verification uses `bash`; the old `verify` tool is removed.
- `grep`, `glob`, and `list` remain dedicated read-only tools for path policy,
  truncation, display, and approval boundaries.
- `read` returns model-visible line-numbered content; the TUI remains compact by
  default.
- `write`, `edit`, and `multi_edit` are the normal editing path. Python, sed,
  heredocs, and shell patching are not the recommended path for ordinary edits.
- Model-visible tool output is bounded with explicit truncation markers.
- Display policy lives in Ion renderers, not in provider-visible history.
- Open-ended user questions remain normal assistant messages for now. A future
  `ask_user` tool should wait for a Canto-level elicitation primitive so it can
  pause/resume safely across TUI, CLI, ACP, and noninteractive hosts.

Canonical behavior lives in `ai/specs/tools-and-modes.md` and
`ai/specs/system-prompt.md`.

## TUI And CLI Shell

The TUI should feel closer to Pi, Claude Code, and Codex than to a dashboard:
flat transcript, compact tool rows, clear progress, minimal settings, and
predictable slash commands.

Important boundaries:

- Live and replayed transcript entries use the same renderer.
- Routine tools are compact by default; full output is opt-in through settings.
- Queued follow-up input is visible, recallable, and submitted once.
- Queued follow-up remains the busy-input default. Opt-in boundary steering is
  limited to active tool calls and is consumed by the native backend before the
  next provider request.
- `/settings` is only durable settings. Provider/model/session identity belongs
  in footer/status, `/session`, `/provider`, `/model`, and `/thinking`.

Scriptable CLI is first-class:

- `-p` / print mode
- text and JSON output
- `--continue`
- `--resume <id>`
- `--agent` for ACP stdio hosts
- provider/model/thinking overrides that are process-local unless the user makes
  an explicit persistent change

## Sessions, Config, And State

Persistent user-editable settings live in `~/.ion/config.toml`.
Mutable runtime choices live in `~/.ion/state.toml`.
Workspace trust lives in `~/.ion/trusted_workspaces.json`.
Durable sessions live in Ion storage backed by Canto session events.
Local branching uses Canto's session lineage primitives. Ion exposes `/fork
[label]`: it branches a materialized session, indexes the child in Ion session
metadata, then switches the TUI into the forked session. `/tree` renders the
current lineage and immediate children from the same ancestry metadata.

Cross-host transfer uses a versioned export/import bundle, not raw SQLite file
sync. The bundle contains Ion session metadata, Canto event envelopes, ancestry
metadata, per-session event checksums, a whole-bundle checksum, and explicit
import conflict behavior. The scriptable surface is `--export-session <file>`
with `--resume <id>` or `--continue`, plus `--import-session <file>`.

`internal/storage` remains an Ion adapter over Canto rather than collapsing into
Canto. It owns Ion-specific session indexes (`cwd`, branch, selected model,
title/preview), input history, TUI display projection, lazy session
materialization, and the portable bundle product shape. Canto should keep
absorbing reusable primitives such as durable event JSON, ancestry, and
effective-history projection, but it should not gain Ion's product metadata or
TUI replay policy.

Startup must not silently persist provider/model choices. Explicit CLI/env
overrides affect the current process. TUI settings and picker actions persist
only the fields they own.

## Safety And Advanced Features

Safety/trust/sandbox work is important but not allowed to destabilize the core
loop. Pi ships without most P3 safety surfaces; Ion can be stronger over time,
but the base agent must be reliable first.

Deferred product layers:

- richer permissions and sandbox UX
- ACP/subscription polish
- memory/wiki
- skills as explicit-install progressive-disclosure modules
- routing and workflow orchestration
- cross-host sync, branching, rewind, and richer rollback
- prompt optimization, eval loops, and provider/runtime caching

These remain long-term goals. They should be reintroduced by clear boundaries
after the baseline stays boring under deterministic, race, tmux, and live smoke
gates.

Skills specifically are not another project-instruction layer. Canto can own
agentskills-compatible registry, routing, and reusable read/manage primitives.
Ion owns `/skills`, install staging, trust prompts, whether a skill is enabled,
and whether skill tools are exposed to the model. The current `/skills` command
and `ion skill list [query]` are read-only local discovery surfaces; `ion skill
install <path>` previews and `ion skill install --confirm <path>` stages and
installs local bundles. The base prompt must not include a skill inventory by
default. `read_skill(name)` exists only behind the opt-in `skill_tools = "read"`
gate; it reads installed local skill bodies by explicit name and stays out of
the default eight-tool surface. `manage_skill`, marketplace install, and
self-extension remain separate write-policy work.

Subagents follow the same progressive-disclosure rule. The default coding
surface remains the eight core tools. `subagent_tools = "on"` opts into the
model-visible `subagent` tool with explicit `summary`, `fork`, and `none`
context modes, built-in explorer/reviewer/worker personas, mode-aware
visibility, and the existing sensitive-tool policy boundary. Memory tools,
background child wakeups, worktrees, subagent communication, and full
alternate-screen swarm mode remain later work.
