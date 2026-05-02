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

## Tool Surface

The P1 native tool surface is intentionally small:

```text
bash, read, write, edit, multi_edit, list, grep, glob
```

Rules:

- Verification uses `bash`; the old `verify` tool is not registered by default.
- `grep`, `glob`, and `list` remain dedicated read-only tools for path policy,
  truncation, display, and approval boundaries.
- `read` returns model-visible line-numbered content; the TUI remains compact by
  default.
- `write`, `edit`, and `multi_edit` are the normal editing path. Python, sed,
  heredocs, and shell patching are not the recommended path for ordinary edits.
- Model-visible tool output is bounded with explicit truncation markers.
- Display policy lives in Ion renderers, not in provider-visible history.

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
- provider/model/thinking overrides that are process-local unless the user makes
  an explicit persistent change

## Sessions, Config, And State

Persistent user-editable settings live in `~/.ion/config.toml`.
Mutable runtime choices live in `~/.ion/state.toml`.
Workspace trust lives in `~/.ion/trusted_workspaces.json`.
Durable sessions live in Ion storage backed by Canto session events.

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
- subagents, routing, and workflow orchestration
- cross-host sync, branching, rewind, and richer rollback
- prompt optimization, eval loops, and provider/runtime caching

These remain long-term goals. They should be reintroduced by clear boundaries
after the baseline stays boring under deterministic, race, tmux, and live smoke
gates.

Skills specifically are not another project-instruction layer. Canto can own
agentskills-compatible registry, routing, and `read_skill` / `manage_skill`
primitives. Ion owns `/skills`, install staging, trust prompts, whether a skill
is enabled, and whether skill tools are exposed to the model. The base prompt
must not include a skill inventory by default.
