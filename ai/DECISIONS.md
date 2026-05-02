# ion Decisions

Distilled architectural principles plus recent decision log.

## Principles

- Native Ion is the product baseline; ACP and subscription bridges are secondary
  compatibility paths.
- Canto owns framework mechanisms: durable events, provider-visible history,
  agent/tool lifecycle, retry/cancel settlement, compaction primitives, and
  provider transforms.
- Ion owns product policy: TUI/CLI shell, commands, settings/state, provider
  choice, display projection, workspace policy, and coding-tool UX.
- Keep the core tool surface small and deliberate. The P1 set is `bash`,
  `read`, `write`, `edit`, `multi_edit`, `list`, `grep`, and `glob`.
- Research is evidence; specs and root files are canonical.
- Do not add a global alternate code path for stabilization. Deferred features
  must be absent or rejected at their owning boundary.
- Scriptable CLI behavior is a first-class regression surface.
- Commit each coherent green slice; do not push without explicit approval.
- Ion is a host over a harness. Canto should expose the headless runtime
  facade; Ion should supply product policy and presentation.

## Recent Log

### 2026-05-02 - Flue validates the harness boundary

Flue is not a TUI replacement for Ion, but its headless programmable shape is a
useful design signal: agent runtime, session, sandbox/env, tools, skills,
commands, and persistence should be explicit harness concepts. Ion should first
drive Canto toward a clearer harness facade, then make `CantoBackend` a thin
adapter over it. Keep the model-facing tool surface small; route future
skills/memory/sandbox behavior behind backend capabilities or explicit
progressive disclosure rather than adding many similar tools.

### 2026-05-02 - Ion keeps its storage adapter

Do not collapse `internal/storage` directly into Canto. Canto owns durable
events, ancestry, and effective history; Ion still needs an application adapter
for workspace/session indexes, input history, lazy materialization, TUI replay
projection, and portable bundle UX. Upstream reusable storage primitives to
Canto over time, but keep Ion product metadata and display policy in Ion.

### 2026-05-02 - AskUser waits for framework elicitation

Ion should not add an Ion-only `ask_user` tool to the default surface. Models
can ask ordinary assistant questions today, and a blocking interaction tool
would need clear behavior for TUI, CLI print mode, ACP hosts, cancellation,
resume, and noninteractive failure. Reopen this as a Canto elicitation
primitive plus Ion TUI renderer, not as a local one-off tool.

### 2026-05-02 - Subagent is opt-in, not default

Ion keeps the default model-visible coding surface at eight tools.
`subagent_tools = "on"` explicitly registers `subagent` as an advanced ninth
tool. Built-in personas are constrained to registered Ion tools, READ mode
hides `subagent`, and fast-slot personas fall back to the primary model when
no fast model is configured so opt-in usage works with a single-model setup.

### 2026-05-02 - Subagent context modes precede exposure

Ion's `subagent` tool boundary now requires explicit context semantics:
`summary` handoff, `fork` parent-history snapshot plus task, or `none` fresh
task-only context. The tool still should not join the default eight-tool
surface automatically; expose it only through a clear opt-in gate after a
deterministic execute smoke.

### 2026-05-02 - Swarm view waits behind subagent context

Ion should not build the alternate-screen swarm/operator view before default
subagent registration is correct. The current near-term surface is inline
Plane B subagent progress plus concise durable child breadcrumbs. Full swarm
mode waits until `summary`, `fork`, and `none` context modes are implemented
and child-session ownership is tested.

### 2026-05-02 - ChatGPT subscription is not ACP

Current OpenAI Codex surfaces support ChatGPT-plan sign-in through Codex
CLI/IDE/app/web and expose Codex-specific app-server/MCP protocols. There is
no supported `codex --acp` bridge in the current CLI. Ion keeps `chatgpt` and
`codex` hidden/deferred, derives no default command for them, and must not
scrape ChatGPT OAuth tokens. Future support would be a Codex app-server adapter,
not Ion's native Canto backend.

### 2026-05-02 - Canto owns request cloning

Provider-history capture, provider-specific request preparation, and future
cache-continuity checks should use Canto's `llm.Request.Clone()` rather than
Ion-local clone helpers. Canto now deep-clones tool parameter schemas,
response-format schemas, cache controls, messages, thinking blocks, and tool
calls. Ion should not add prompt/KV cache machinery in this slice.

### 2026-05-02 - ripgo is promising but not Ion-ready

`tk-03hf` measured local ripgo against Ion's current ripgrep-backed search.
ripgo was faster on a small Ion `internal/` grep benchmark, but the closest CLI
flags still searched `.git/config` in a fixture where Ion's `rg --glob
'!.git/**'` path excludes it, and ripgo CLI lacks the `rg --files` surface Ion
uses for `glob`. Keep ripgrep as the baseline until ripgo has semantic parity
tests and direct `walk`/`ignore` integration for file listing.

### 2026-05-02 - manage_skill is a protected write surface

Future `manage_skill` is not a normal default tool. It requires
`skill_tools = "manage"`, write-capable mode, local `~/.ion/skills` target,
hard user approval for mutations even in AUTO, mutation audit entries, and
trash-based removals. If those gates are not present, host-owned
`ion skill install --confirm` remains the mutation path.

### 2026-05-02 - skill install is explicit local staging

Ion supports `ion skill install <path>` as validation/preview and
`ion skill install --confirm <path>` as the install action. Installs are local
only, stage before rename, validate through agentskills, reject symlinks/special
files and overwrites, and never run fetched scripts. Remote marketplace install
remains deferred.

### 2026-05-02 - read_skill is opt-in progressive disclosure

Ion exposes `read_skill(name)` only when `skill_tools = "read"` is enabled. It
does not change the default eight-tool coding surface and does not add a skill
inventory to the prompt. `manage_skill`, marketplace install, and
self-extension remain behind later explicit write/trust gates.

### 2026-05-02 - Ion headless ACP mode is an adapter

`ion --agent` runs Ion as an ACP stdio agent for external hosts, but it is not
a second native loop. It reuses the existing `AgentSession` runtime boundary and
translates ACP initialize, new/load session, prompt streaming, tool updates,
approval requests, cancellation, and session mode updates around that boundary.

### 2026-05-02 - Cross-host sessions use portable bundles

Ion should not expose raw SQLite sync as the product surface for moving
sessions between machines. Cross-host transfer uses a versioned JSON bundle
that preserves Ion session metadata, Canto event envelopes, Canto ancestry
metadata, per-session event checksums, a whole-bundle checksum, and explicit
import conflicts. Canto owns portable event/ancestry primitives; Ion owns the
CLI/TUI import/export workflow.

### 2026-05-02 - Session branching uses Canto lineage

Ion should not invent a second session tree. Local branching uses Canto
`BranchSession`/ancestry metadata and Ion only adds product indexing plus TUI
commands. `/fork [label]` is the first branch surface; cross-host transfer and
tree browsing build on the same lineage rather than copying transcript display
rows.

### 2026-05-02 - Skills are explicit-install progressive disclosure

Ion should not expose skills as always-on prompt text or as default coding
tools. Canto owns agentskills-compatible registry, routing, and read/manage
primitives; Ion owns `/skills`, install staging, trust prompts, and model tool
exposure. `read_skill` is opt-in progressive disclosure; `manage_skill` and
marketplace install require explicit user enablement and write-policy gates.

### 2026-05-02 - Ion keeps product tool wrappers

Canto's stable `coding` tools are useful framework primitives, but Ion should
not directly expose them as its default model-facing tools. Ion wrappers own
short tool names, line-numbered reads, ripgrep search, checkpoints, sandbox and
mode policy, compact display, and recovery-focused edit errors. Adopt Canto
pieces only where they preserve that product contract.

### 2026-05-02 - Thinking controls are capability-filtered

Ion does not send reasoning/thinking request fields based on provider names.
Canto exposes typed model capabilities for named efforts, disable support, and
budget-backed thinking; Ion filters `/thinking` through those capabilities.
Unknown or generic OpenAI-compatible endpoints default to no reasoning params
unless explicitly configured.

### 2026-05-02 - Busy steering is tool-boundary only

Queued follow-up stays the default busy-input behavior. Opt-in steering is
limited to active tool-call boundaries, where Ion can hand text to the native
backend and the backend can consume it as Canto context before the next
provider request. Streaming final answers, compaction, ACP, inactive sessions,
and uncertain cases fall back to queued follow-up.

### 2026-05-02 - Subagents need explicit context modes

Ion should not register the `subagent` tool by default until child context
transfer is explicit and tested. The future schema should distinguish compact
summary context, forked parent-history snapshots, and no inherited context.
Canto should own reusable child-session/history-snapshot primitives; Ion should
own personas, tool exposure, display, and product policy.

### 2026-05-02 - Background bash stays one tool

Ion should add background command monitoring as an extension of `bash`, not as
separate default `bash_output`, `bash_kill`, and `monitor` tools. Foreground
commands remain simple. Background mode is explicit, returns live session job
ids, uses the same policy/sandbox posture, and does not promise process
survival across app restart in the first implementation.

### 2026-05-02 - Edit surface stays split after I2 evaluation

Pi's merged `edit(path, edits[])` is the best future simplification candidate,
but Ion should keep `write`, `edit`, and `multi_edit` for the current I4
surface. The split tools are already hardened around exact replacement, CRLF/BOM
preservation, line-numbered errors, expected replacement counts, and atomic
validation. A merged edit surface needs eval evidence across single-file,
multi-file, overlap, duplicate, CRLF/BOM, cancellation, and
provider-compatibility cases before replacing working tools.

### 2026-05-02 - AI context becomes design-first

The next Ion work stream starts by pruning `ai/` and rewriting root files around
the full product design, not just the old core-loop flag. Root `ai/` stays to
five canonical files. Topic docs are evidence only.

### 2026-05-02 - Native baseline is single-path

The old global stabilization split is removed. The current native P1 behavior
is the normal path: eight default tools, compact tool display,
provider-history request capture, and deferred ACP/MCP/memory/subagent/trust/
policy surfaces rejected at their owning boundaries.

### 2026-05-01 - Tool research is distilled into specs

Tool-surface and prompt-budget research stays as evidence. Durable behavior
lives in `ai/specs/tools-and-modes.md` and `ai/specs/system-prompt.md`.

### 2026-05-01 - Dedicated search tools stay

Ion keeps `grep`, `glob`, and `list` as dedicated read-only tools for display,
truncation, path policy, and approval boundaries. `rg` semantics remain the
near-term baseline; ripgo is a later benchmarked integration.

### 2026-05-01 - Structured edits stay

Ion keeps `edit`, `multi_edit`, and `write` for the current tool surface.
Expected replacement counts, line-numbered ambiguity errors, CRLF/BOM-safe
matching, and atomic validation are the reliability priorities. A merged
`edit(edits[])` surface is deferred.

### 2026-05-01 - Model-visible truncation must be explicit

Native tool results pass through one shared output limiter. Truncated model
observations include an explicit marker with byte counts and recovery guidance.

### 2026-05-01 - Read output carries line numbers

The model-visible `read` result includes cat-style line numbers for edit
precision. The TUI still summarizes routine read rows by default.

### 2026-05-01 - Verification uses bash by default

The dedicated `verify` tool is removed from the default native registry.
Ordinary tests/builds/lints run through `bash`; structured verification is
deferred until an eval/RLM feature needs it.

### 2026-05-01 - Roadmap sequencing

Ion proceeds as: stable native agent, minimal TUI/CLI shell, safety/product
table stakes, then advanced framework and SOTA surfaces. Canto reopens only for
concrete framework-owned failures found by Ion.

### 2026-04-30 - Busy input queues by default

Queued follow-up remains the default. Active-turn steering is allowed only in
the opt-in tool-boundary path so Ion never inserts user text into invalid
provider-visible history.

### 2026-04-27 - Core parity gates feature work

Pi/Codex/Claude parity is a staged reliability baseline, not a feature
checklist. Native submit/stream/tool/cancel/error/persist/replay correctness
blocks provider polish, ACP, subscriptions, skills, branching, and routing.

### 2026-04-27 - Runtime retry until cancellation

Transient provider/network failures retry until user cancellation by default.
Canto owns retry mechanics; Ion owns the setting, visible retry status, and
persisted status rows.

### 2026-04-27 - Thinking controls are capability-driven

Ion exposes a small reasoning vocabulary. Canto/provider adapters translate it
to provider-native request fields or omit unsupported parameters.
