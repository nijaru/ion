# Coding-loop survey — 2026-09-14

Scope: how leading coding agents instruct the model, declare and shape tools, represent edits,
manage context/compaction, decide to stop, delegate, and gate shell/writes — plus what public
evaluations actually measure. This is the reading half of the effectiveness research listed in
[`../research.md`](../research.md) §5: it records **known** behaviour so the R7/R8 provider,
tool-execution, context and evaluation boundaries are grounded, not so Ion copies anything.
[`DESIGN.md`](../../DESIGN.md) owns contracts; [`../decisions.md`](../decisions.md) owns choices. It
does not repeat the durable-kernel, topology and storage findings in [`../research.md`](../research.md),
[`pico-core-and-ai-boundaries-2026-09-12.md`](pico-core-and-ai-boundaries-2026-09-12.md) and
[`agent-topology-context-2026-09-12.md`](agent-topology-context-2026-09-12.md); cite those.

## 1. Evidence rules

Findings use the strongest evidence actually read, in this order:

| Mark | Meaning |
|---|---|
| **[SRC rN]** | Source code read at pinned revision `rN` (§2.0). Establishes what that revision implements. Strongest. |
| **[DOC]** | Vendor documentation. Establishes the publisher's claimed public behaviour, not internals. |
| **[BENCH]** | Published benchmark/ablation with reported conditions; direction and size given where reported. |
| **[3P]** | Credible third-party observation, paper, or quoted artefact (not vendor marketing). |
| **[ANEC]** | Anecdote/unsourced. Used only to explain practice, never as a claim. |

Rules: closed-source behaviour seen through docs/output is **evidence of behaviour, not internals**
(Cursor and Claude Code internals are not verifiable by reading; claims about them stay `[DOC]`). No
unsourced numbers; first-party or single-model figures are marked and their confounds listed in §3.6.
Revisions are pinned in §2.0; docs accessed 2026-09-14 (America/Los_Angeles). Another project's
mechanism earns inclusion by answering an unresolved question, not by popularity; nothing here is a
parity target. To use a finding: locate it in §2, check its mark, then §3 for measurement, §4 for
agreement/conflict, §5 for the Ion boundary it touches.

## 2. Harness survey

### 2.0 Pinned revisions read

| Source | Revision read | Date |
|---|---|---|
| OpenAI Codex CLI (`openai/codex`) | `99b3ab2131a8672089fd7d78da62483187ba1122` | 2026-09-14 |
| Google Gemini CLI (`google-gemini/gemini-cli`) | `9c1b0a610534d6f8120964cf2672c07807d8fc90` | 2026-09-11 |
| Aider (`Aider-AI/aider`) docs | `5dc9490bb35f9729ef2c95d00a19ccd30c26339c` | 2026-05-22 |
| Claude Code docs (`code.claude.com/docs`) | living docs | 2026-09-14 |
| Cursor docs + Composer self-summarization post | living docs/blog | 2026-09-14 |
| OpenHands SDK docs + platform paper | living docs; arXiv 2407.16741 | 2026-09-14 |
| SWE-agent | arXiv 2405.15793 (NeurIPS 2024) + living docs | 2026-09-14 |

### 2.1 Codex CLI — `[SRC]` unless noted

**Instructions / project context.** `AGENTS.md` discovery walks up from cwd to a project root
(default marker `.git`, configurable `project_root_markers`), concatenates every `AGENTS.md` from the
project root **down to cwd** plus a user-level file (separator `--- project-doc ---`), supports
`AGENTS.override.md` and configurable fallback filenames, and does not walk past the root. The repo
ships model-specific prompt files (`gpt_5_1_prompt.md`, `gpt_5_2_prompt.md`,
`gpt-5.2-codex_prompt.md`), so guidance is versioned per model, not one neutral prompt. `[SRC]`

**Tools / result shaping.** Shell runs through unified-exec with a head/tail buffer; tool and exec
output is truncated by a byte **or** token budget, in the **middle**, preserving head and tail and
prefixing an explicit notice (`Warning: truncated output (original token count: N)` /
`Total output lines: M`). The exact default budget was not read; do not quote one. `[SRC]`

**Edits.** `apply_patch` is a **freeform** tool (the model prompt says so explicitly) with a bundled
Lark grammar (`*** Begin Patch`/`*** Add File`/`*** Delete File`/`*** Update File`/`*** Move to`/`@@`
hunks/`+`/`-` lines/`*** End Patch`) applied by exact match. `[SRC]` "3 lines of default context" and
shell invocability come from an older template seen only as a search snippet — `[3P]`, not `[SRC]`.

**Stop.** The app-server settles turns after a model response and tool results; loop-until-no-tool-calls is `[DOC]` (internal predicate not read).

**Compaction.** A first-class task with a `CONTEXT CHECKPOINT COMPACTION` template asking for a
handoff summary (progress, decisions, constraints, remaining work, critical data); the injected
prefix tells the next model it inherits the previous model's **tool state**. `thread/compact/start`
streams progress. Separate local and remote compaction paths exist; the trigger threshold was not
read from source. `[SRC]`/`[DOC]`

**Subagents.** V2 spawns by thread with `spawn_agent`/follow-up/send/wait/interrupt/list and
`fork_turns = none | all | N`, already recorded in [`../research.md`](../research.md) R3/R4 and the
topology note; the app-server adds an `auto_review` mode routing approvals to a prompted subagent.
`[SRC]`/`[DOC]`

**Permissions.** Approval policies `never`/`on_request`/`unless_trusted`/`granular`; sandbox modes
`read_only`/`workspace_write`/`danger_full_access`. `workspace_write` permits reads and writes under
`cwd`/`writable_roots`, elsewhere needs approval. Escalation splits a command at shell control
operators (`|`, `&&`, `||`, `;`, subshells) and evaluates each segment; redirection/substitution/
wildcards are excluded from prefix matching and broad prefixes like `["python3"]` are banned. Patch
safety auto-approves only when the write is constrained to writable paths and a sandbox is available.
`[SRC]`

### 2.2 Claude Code — `[DOC]`

**Instructions.** System prompt; `~/.claude/CLAUDE.md`; project-root `CLAUDE.md`; nested
subdirectory `CLAUDE.md` (loads when a file there is read); path-scoped `.claude/rules/` with
`paths:`; skill descriptions (bodies only on invocation); auto memory `MEMORY.md` (first 200 lines or
25 KB). Docs state imports load at launch and therefore do not reduce context. After compaction the
project-root `CLAUDE.md` is re-read and re-injected; nested `CLAUDE.md`/`paths:` rules are lost until
a matching file is read; invoked skill bodies are re-injected capped at 5,000 tokens/skill and
25,000 total. `[DOC]`

**Tools / result shaping.** Read/Grep/Glob, Bash, Edit/Write, Agent (subagents), WebFetch/WebSearch,
Monitor (background command streaming), Workflow (subagent orchestration). MCP tool **names** load,
full schemas are deferred and fetched via `ToolSearch` (`ENABLE_TOOL_SEARCH`). Bash valid output is
inline up to ~30,000 characters by default, spilling to a file path plus preview past that (file
truncated past 64 MiB); a failing command gets an inline ~10,000-character head-and-tail excerpt with
no path; `BASH_MAX_OUTPUT_LENGTH` (ceiling 150,000) and `bashOutputMaxChars` (up to 128,000) tune it.
Default timeout 120 s, ceiling 10 min; a timed-out command is moved to the **background**, and the
result says so. Hook output over 10,000 characters spills to a file. `[DOC]`

**Edits.** `Edit` requires `old_string` to appear **exactly** (whitespace included); one character
off misses. `Write` rewrites the file. `[DOC]`

**Stop.** The loop repeats until a response has **no tool calls**; `max_turns` counts tool-use turns
and `max_budget_usd` caps spend. `[DOC]`

**Compaction.** `/compact` replaces the conversation with a structured summary; auto-compaction
triggers near 95 % of the window, lowered via `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`. Subagent transcripts
are separate files, unaffected by main-conversation compaction, with their own auto-compaction. `[DOC]`

**Subagents.** `Agent` starts a subagent with its own context window; only the final result returns.
Background by default; `maxTurns`; optional persistent memory dir; fork mode inherits the full parent
conversation. Agent teams (`SendMessage`, `Workflow`) are experimental. `[DOC]`

**Permissions.** Manual mode starts read-only; Read/Grep/Glob auto inside the working dir, Bash
except a read-only allowlist, Edit/Write, WebFetch/WebSearch. Rules evaluate **deny → ask → allow**,
first match wins; Bash/WebFetch allows persist per repository, file-edit approval lasts until session
end. Working-directory boundary; optional sandboxed Bash; auto mode substitutes a classifier. Network
commands are not auto-approved. `[DOC]`

### 2.3 Aider — `[DOC]` (docs at pinned revision)

**Instructions / context.** A repo map of signatures/definitions for the whole repo is sent with each
request; users add files or `CONVENTIONS.md` read-only. `[DOC]`

**Tools / result shaping.** No tool-calling protocol: the model returns edits as **text**; `/run`,
`/lint`, `/test` run on request or automatically. Built-in per-language linters run on edited files by
default; `--lint-cmd` and `--test-cmd`/`--auto-test` feed failures back for repair. `[DOC]`

**Edits.** Four formats: `whole` (full file), `diff` (`<<<<<<< SEARCH`/`=======`/`>>>>>>> REPLACE`),
`diff-fenced` (path inside the fence; Gemini family), `udiff` (simplified unified diff). Chosen per
model; **unknown models default to `whole` "since it is the easiest format for an LLM to use"**. `[DOC]`

**Stop / git.** Aider auto-commits each change (committing pre-existing dirty files first) so `/undo`
and git history are the safety net; `--no-auto-commits`/`--no-git` disable it. `[DOC]`

**Context growth.** Reviewed docs describe managing context by adding/dropping files and the repo
map, not automatic summarisation. Absence in the reviewed docs, not proof of absence in code. `[DOC]`

**Subagents / permissions.** No delegation surface; architect mode splits planning and editing across
two models, a two-model pipeline rather than delegation. No approval/sandbox gate in the reviewed
docs; the safety contract is git. `[DOC]`

### 2.4 SWE-agent — `[BENCH]` (paper) otherwise `[DOC]`

**ACI.** Thesis: a purpose-built interface beats a human shell UI. Decisions: file viewer showing at
most **100 lines** with line numbers and scroll/goto; search that lists **only files with at least
one match** because per-match context "proved to be too confusing"; edit by start line/end
line/replacement, backed by a **syntax linter** that discards invalid edits and returns the error with
before/after content; explicit "command ran successfully and did not produce any output"; malformed
generations get an error and are retried, with past error messages omitted except the first. `[DOC]`

**Context.** History processors window observations; the sweep uses "last 5" vs "full". `[BENCH]`
**Stop / permissions.** The agent issues `submit` to produce a patch; runs in a container with no
interactive approval UX (benchmark setting). `[DOC]`

**Ablation (SWE-bench Lite, 300-instance subset).** Shell-only 11.00 % with demonstration vs 7.33 %
without; edit action 15.0 % → 18.0 % with linting; search "summarized" 18.0 % vs "iterative" 12.0 %;
file viewer 30 lines 14.3 %, **100 lines 18.0 %**, 400 lines 17.0 %, full file 12.7 %; context with
demo 18.0 % vs without 16.3 % vs full history 15.0 %. Sweep on 37 dev instances: "last 5" beat "full"
(GPT-4 Turbo 15.1 vs 14.1; Claude 3 Opus 8.1 vs 5.4). Full SWE-bench GPT-4 Turbo 12.47 %; Lite
18.00 %; Claude 3 Opus 13.00 %. `[BENCH]`

### 2.5 OpenHands — `[DOC]`/`[BENCH]`

**Loop.** Stateless reasoning-action `step()`: pending actions → optional **condenser** → LLM query →
security analysis → tool execution → observation events. `[DOC]`

**Edits / shaping.** `str_replace_editor` supports `view` (`cat -n`-style), `create`, `str_replace`
(exact `old_str`, must be unique or the edit is refused), `insert`, `undo_edit`; long output is
truncated and marked `<response clipped>`. `[3P]` (quoted schema) / `[DOC]`.

**Compaction.** `LLMSummarizingCondenser` triggers above a configured `max_size` of events, always
keeps the first `keep_first` events (system + initial user), and replaces dropped events with an LLM
summary. `[DOC]`

**Permissions.** A configurable security analyzer scores proposed actions; unconfirmed actions set
`WAITING_FOR_CONFIRMATION`. `[DOC]`

**Benchmarks (first-party, subset of SWE-bench Verified).** Condensation 54 % vs 53 % baseline while
halving per-turn API cost and turning quadratic context growth linear; inference-time scaling with a
trained critic 60.6 % → 66.4 % with five attempts. `[BENCH]` (vendor-reported)

### 2.6 Gemini CLI — `[SRC]`

**Instructions.** `GEMINI.md` is hierarchical and cwd-relative: scanned in the current directory and
ancestors up to a trusted root, plus subdirectories below cwd (respecting ignore patterns); filename
configurable via `context.fileName`; an import processor expands `@file.md`; `/init` generates one.
`[SRC]`/`[DOC]`

**Tools.** `run_shell_command` (interactive/background; returns stdout/stderr/exit code/background
PIDs; manual confirmation), `read_file`, `read_many_files` (`@`), `glob`, `grep_search`,
`list_directory`, `replace`, `write_file`, `ask_user`, `write_todos`, experimental tracker tools, MCP
resources, `activate_skill`, web search/fetch. `[SRC]`

**Edits.** `replace` takes `old_string`/`new_string` and by default requires **exactly one**
occurrence (`allow_multiple` opts out); `write_file` overwrites/creates. Both require manual
confirmation. `[SRC]`

**Context.** `/compress` replaces the entire chat context with a summary;
`model.compressionThreshold` is the fraction of context usage that triggers automatic compression,
**default 0.5**. `[SRC]`

**Checkpointing / permissions.** Before a filesystem-modifying tool, the CLI snapshots the project
into a git shadow repo and stores history/tool calls as JSON under
`~/.gemini/tmp/<project_hash>/checkpoints`; `/restore` reverts. Mutators require confirmation;
sandboxing, trusted folders, `--allowed-tools` and a policy engine exist. `[SRC]`

**Subagents.** No built-in subagent/delegation tool appeared in the reviewed tools reference. `[SRC]`

### 2.7 Cursor — `[DOC]` (closed source: behaviour only)

**Instructions.** `.cursor/rules/*.mdc` with `description`/`globs`/`alwaysApply`, user/team/project
scopes; plain `.md` without frontmatter is ignored; the CLI also reads `AGENTS.md` and `CLAUDE.md` as
rules. Applied rule contents are injected at the start of the model context. `[DOC]`

**Tools.** File edit, codebase search, terminal execution, web search, fetch-rules, file read,
browser, image generation, ask-questions; "no limit on the number of tool calls". `[DOC]`

**Context.** Older messages are automatically summarised near window-full; large files/folders are
"smart condensed" to structural elements (signatures/classes/methods), reduced to name-only
("significantly condensed") or excluded. The context tray reports tokens by category. `[DOC]`

**Self-summarization.** Composer is trained with "compaction-in-the-loop": at a fixed token trigger
the harness asks the model to summarise its own context, and the reward trains the summaries. Reported:
~1,000-token summaries vs >5,000-token prompted baseline, ~50 % less compaction error on internal
CursorBench at 40 k/80 k triggers, KV-cache reuse. `[BENCH]` (vendor, their model + benchmark)

**Checkpoints / permissions.** Codebase snapshots before significant changes (local, separate from
git); reverting affects files, not messages. Approval internals, tool-call enforcement and subagent
policy are not observable; subagents appear only as a context category and background agents. `[DOC]`
— thin.

### 2.8 Pi 2 / Pico

Cross-reference, not repeated:
[`pico-core-and-ai-boundaries-2026-09-12.md`](pico-core-and-ai-boundaries-2026-09-12.md) records
Pico2 as the minimal-harness design reference and specifically that its normative spec **gates
provider generation/system integration and complete built-in tool schemas**. Pico2 therefore supplies
no implementation evidence for edit representation, tool-result shaping or compaction policy; ordinary
Pi is practical product evidence only. `[3P]` (Ion's prior review)

## 3. Empirical evidence

### 3.1 Scaffold/model ablations on SWE-bench-style tasks

| Finding | Direction / size | Confounds |
|---|---|---|
| Purpose-built ACI beats shell-only | Lite 300-instance: 11.00 % shell vs 18.00 % SWE-agent (GPT-4 Turbo) | 2024 models, benchmark-tuned harness, 300-subset |
| Demonstrations matter | Shell-only 11.00 % with demo vs 7.33 % without | prompt content not isolated |
| Lint/syntax guardrail on edit | 15.0 % → 18.0 % | one model/benchmark |
| Search result shaped to file-list only | 18.0 % "summarized" vs 12.0 % "iterative" | model-specific; different interaction |
| File-view window is non-monotonic | 30 lines 14.3 %, **100 lines 18.0 %**, 400 lines 17.0 %, full 12.7 % | one model; optimum model-specific |
| Execution-history window beats full | "last 5" 15.1 vs 14.1 (GPT-4 Turbo); 8.1 vs 5.4 (Claude 3 Opus), 37 dev instances | small sample, sweep |
| A non-agent pipeline is competitive | Agentless 32.00 % Lite at ~$0.70, best open-source agent at the time; fixed localize→repair→validate, no agent-chosen actions | 2024 models; Lite split; still LLM-heavy with retries |

### 3.2 Harness-level effect (strongest single result here)

*Same Model, Different Harness* (arXiv 2608.26218, 2026-08-26) fixes model and tasks and varies one
harness: control supplies the full conversation in time order; treatment keeps the same record but
mechanically shortens **older tool results** as context fills and responds to repeated/stalled work.
On a tight 20,480-token window over 169 SWE-bench Verified tasks with a 480 s attempt cap, mean
per-task fail-to-pass fraction rose **28 % → 49 %** and complete solutions **43 → 72**; the same
frozen treatment improved both endpoints for three more models; wide Qwen3.6 windows were close on
Verified/Pro. This is the clearest published evidence that tool-result shaping is a first-order
harness effect, not plumbing. `[BENCH]` Confounds: one harness pair, specific tasks/window, no vendor
harness tested.

### 3.3 Compaction and context management

- OpenHands condenser: 54 % vs 53 % on a SWE-bench Verified subset, ≤2× lower per-turn cost,
  quadratic → linear growth. Neutral-to-positive performance, positive cost. `[BENCH]` (vendor)
- Cursor self-summarization: ~50 % less compaction error than a tuned prompted baseline on internal
  CursorBench, ~1,000- vs >5,000-token summaries. Positive, but their model + benchmark. `[BENCH]`
- Microsoft *Less Context, Better Agents* (2026) found selective pruning + compact summarisation beat
  full-history retention on a long-horizon tool-use task; already recorded in
  [`../research.md`](../research.md) and the topology note — cite, do not re-derive. `[3P]`
- Claude Code, Codex and Gemini all implement triggered summarisation with different trigger points
  (Gemini default 0.5 of window; Claude ~0.95). No same-model cross-harness head-to-head found. `[DOC]`

### 3.4 Edit representation

- Aider (first-party), 89-task Python refactoring benchmark, `gpt-4-1106-preview`: SEARCH/REPLACE
  20 % → udiff 61 %; lazy-comment rate 12 tasks → 4. `[BENCH]` Confounds: Aider's own benchmark, one
  model version, single-turn; Aider notes 28 % of tasks exceeded June GPT-4's 8 k window (ceiling 72 %).
- An ICLR-2026 submission under review reports an Aider ablation finding **similar single-turn
  correctness** across `udiff`/`diff`/`whole` in a multi-turn correctness/security suite — edit format
  mattered less than Aider's single-turn benchmark suggests. `[3P]` (preprint; read only as a snippet).
- SWE-agent replaced line-range editing with linting guardrails (+3.0 on its subset); Claude `Edit`,
  Gemini `replace` and Codex `apply_patch` all require exact-match anchors and refuse ambiguous or
  missing matches. Convergence on anchored edits is strong; the winning format is not established.

### 3.5 Retries, test feedback, inference-time scaling

- OpenHands: 60.6 % single rollout → 66.4 % with five attempts plus a trained critic (SWE-bench
  Verified). `[BENCH]` (vendor) Confound: extra test-time compute and a trained reranker, not a loop
  improvement.
- SWE-agent and Aider both feed lint/test failures back and discard invalid edits; the SWE-agent
  ablation (15.0 → 18.0) is the only quantified effect found for the guardrail itself. `[BENCH]`

### 3.6 Delegation

- Anthropic multi-agent research system (2025-06-13): Opus 4 lead + Sonnet 4 subagents beat
  single-agent Opus 4 by **90.2 %** on an internal **research** eval; multi-agent used ~15× the tokens
  of chat and token usage alone explained ~80 % of variance. `[BENCH]` (vendor, non-coding, unequal
  budget).
- Coding-specific cooperation evidence is negative/mixed, already in
  [`agent-topology-context-2026-09-12.md`](agent-topology-context-2026-09-12.md): CooperBench found
  two cooperating coding agents often underperform one at equal workload, and Google's scaling study
  found every multi-agent variant degraded a sequential planning benchmark (39–70 %) while centralized
  orchestration helped a parallelizable task. `[3P]` Cite that note.
- No public controlled result shows subagent delegation improving coding outcomes at equal token budget.

## 4. Cross-cutting findings

### 4.1 What independent sources agree on

1. **One project-instruction file with hierarchical/implicit loading is near-universal.** `AGENTS.md`
   (Codex), `CLAUDE.md` + path rules (Claude Code), `GEMINI.md` (Gemini CLI), `.cursor/rules` +
   `AGENTS.md`/`CLAUDE.md` (Cursor). Agreement about the *convention*; **no controlled measurement
   that the file improves outcomes was found** (§4.3).
2. **Anchored, exact-match edits dominate whole-file writes at large file sizes.** Codex hunks,
   Claude `Edit`, Gemini `replace` (unique by default), OpenHands `str_replace` (unique), Aider
   `diff`/`udiff`; whole-file is the weak-model fallback.
3. **Tool results are deliberately shaped, not passed through raw.** Truncation notices + spill-to-file
   (Codex, Claude), windowing (SWE-agent, Claude failure excerpt), structural condensation (Cursor),
   summarisation (OpenHands, Claude, Codex, Gemini). Direction supported by §3.2; OpenHands and
   *Less Context* are neutral-to-positive.
4. **Validation guardrails reduce error propagation.** Lint/syntax check on edit (SWE-agent +3.0;
   exact-match rejection in Claude/Gemini) and lint/test feedback loops (Aider, OpenHands critic).
5. **A turn ends when the model stops requesting tools**; explicit budgets/turn caps are common
   (Claude `max_turns`/`max_budget_usd`; Gemini loop detection; OpenHands step loop). Every surveyed
   agent also ships a plan/todo surface (Codex `update_plan`, Gemini `write_todos`, Cursor `/goal`,
   Claude tasks); its effect on outcomes is unmeasured (§4.3).
6. **Reads are cheap; writes/shell need approval and shell is sandboxed.** All surveyed products
   separate read-only from mutating actions; Codex adds per-segment command evaluation and prefix
   allowlists, Claude persist-per-repo rules, Gemini a policy engine.
7. **Every major product ships some subagent/delegation surface** (Claude `Agent`/teams, Codex
   `spawn_agent`, Cursor subagents, OpenHands multi-agent; SWE-agent is the exception). Convergence on
   the feature is not evidence of benefit (§3.6).

### 4.2 Where sources disagree, with reasons

- **How to compact.** Prompted summarisation (Claude, Codex, OpenHands, Gemini, Cursor baseline),
  trained self-summarisation (Cursor Composer), sliding window, latent/vector compaction. Structural:
  a vendor that trains its own model can move compaction into the model; a provider-neutral harness
  cannot. No same-model comparison exists.
- **Where compaction runs.** Harness task (OpenHands, Claude) vs provider/server-side (Codex remote
  compaction) vs model training (Cursor) — different deployment constraints, no settled ranking.
- **Subagent context inheritance.** Claude defaults subagents to fresh context (forking opt-in);
  Codex's current `spawn_agent` defaults to full-history fork; Anthropic recommends isolated context
  for parallel work. The topology note already concludes fresh is the conservative default with
  inheritance explicit.
- **How aggressive result shaping should be.** SWE-agent found *more* search context confused the
  model and returned a file list only; Cursor invests in keeping more via structural condensation.
  Both may be right for different models and result types.

### 4.3 Widely practised but currently unevidenced (flagged)

- **Project instruction files improving task outcomes.** Universal, unmeasured here.
- **Deferred/"tool search" tool discovery.** Documented in Claude Code and the OpenAI Agents API
  ([`../research.md`](../research.md) R8), but no ablation found showing it changes outcomes rather
  than only startup tokens.
- **Repo maps.** Aider documents/claims value; no independent ablation found.
- **"More context/agents is better."** Contradicted for context (§3.2, *Less Context*) and unsupported
  for coding delegation at equal budget (§3.6). Aider also measured that widely circulated "emotional
  appeal" prompt tricks **lowered** scores. `[BENCH]`
- **Per-model prompt tuning and plan/todo tools.** Cursor tunes per model, Codex ships model-specific
  prompts, and plan/todo tools are universal (Codex, Gemini, Cursor, Claude); no ablation found for
  either.

## 5. Implications for Ion

Each line: **adopt / test / reject / unknown**, the affected boundary, and the link.

1. **Adopt: freeze the resolved instruction + project-context projection at the request boundary.**
   Codex's AGENTS.md merge order and per-model prompt files show the resolved prompt is an input, not
   a constant. [`DESIGN.md` §13](../../DESIGN.md) already requires freezing the resolved request; make
   the instruction layers and their revisions part of that record. Affects R7. **Flag: this extends
   D13** — the frozen request must carry the resolved instruction projection revision, not only
   model/settings/cutoff/tools.
2. **Test: `AGENTS.md`-compatible hierarchical discovery (project root → cwd, local override) rather
   than inventing a format.** Evidence is convergence only (§4.1.1). Cheapest test in §6.
3. **Test, do not adopt: a specific edit representation.** Anchored exact-match with a whole-file
   fallback is the defensible baseline (§4.1.2); the winning variant is model-dependent (§3.4).
   Reject making Aider's udiff, Codex's V4A envelope, or a unique-`old_str` form canonical without
   Ion's own measurement. Affects the tool schema in [`DESIGN.md` §10](../../DESIGN.md) and the
   execution-environment pass (§14).
4. **Adopt: bounded tool results with explicit truncation metadata and spill-to-artifact.** Codex and
   Claude both emit a machine-visible "truncated, original size N" notice and a file path; Ion needs
   R6/R8 byte caps and has an `Artifact` boundary ([`DESIGN.md` §17](../../DESIGN.md)). The tool result
   type should carry the truncation fact so a model is never silently given a prefix.
5. **Test: how aggressively to shape, and whether structural condensation beats plain truncation.**
   SWE-agent's 100-line window beat full file; Cursor's condensation is a different mechanism; *Same
   Model* says shaping matters but not which shape. Interface: file-read and search result builders
   (R8 measurement, not a kernel change).
6. **Adopt: append-only head/edit compaction is already accepted (D3); add a configurable trigger and
   safe placement as an ordinary built-in task.** Gemini's 0.5 and Claude's ~0.95 defaults show
   triggers are policy, not architecture ([`DESIGN.md` §7](../../DESIGN.md)). **Unknown → test:**
   whether triggered summarisation beats mechanically shortening older tool results (§3.2) on Ion's
   tasks; do not assume summarisation is the better of the two.
7. **Unknown: provider-side or trained compaction.** Codex remote compaction and Cursor
   self-summarisation are real but not implementable in a provider-neutral harness without provider
   support. A watch item; it does not change the derived-context contract.
8. **Test: refuse an edit that fails a cheap syntax/format check and return the error to the model.**
   SWE-agent's only quantified guardrail (15.0 → 18.0). An execution-environment behaviour, not a
   kernel concern ([`DESIGN.md` §14](../../DESIGN.md)); it also fits D11's per-tool retry opt-in
   because the failed edit was never dispatched.
9. **Test: an explicit lint/test feedback loop as a bounded turn policy.** Aider and SWE-agent both
   feed failures back; R8 already schedules a test-feedback coding regression. Keep it separate from a
   tool's own retry-safety class (D11).
10. **Adopt: explicit step/cost/deadline limits and the stop rule "model requested no tools".** Already
    required by R8; configuration stores limits but the generation loop does not yet enforce them.
    `TurnTemplate` selects task kind/schema/input, not budget enforcement. The survey confirms it is the common
    contract, nothing more.
11. **Adopt: per-segment command evaluation and explicit prefix/approval rules for shell.** Codex
    splits at `|`, `&&`, `||`, `;`, subshells and bans broad prefixes; Claude persists rules per repo;
    Gemini has a policy engine. This refines [`DESIGN.md` §14](../../DESIGN.md)'s "approvals bind the
    exact prepared invocation" with the concrete question of *what a rule names* (P3 pass).
12. **Reject as a default: subagents.** No controlled coding result shows benefit at equal budget
    (§3.6); the topology note already concludes fresh/bounded delegation; D8 puts the single-agent
    baseline first. Keep retained workers optional and measure them after M2.
13. **Test: inference-time scaling as a separate budget axis.** OpenHands' 60.6 → 66.4 % with a critic
    shows attempts buy success; evaluate it as a deliberate cost multiplier against the baseline, not
    as a loop feature.
14. **Adopt: per-model request assembly without a kernel change.** Cursor tunes instructions/tools per
    model and Codex ships model-specific prompts; [`DESIGN.md` §13](../../DESIGN.md) already separates
    provider adapters from the kernel. This is an `ion-ai`/R7 point.

Decisions affected: **D13** should be tightened in implementation (frozen request names the resolved
instruction/tool projection); **D3** needs no change for harness-side compaction but should record
provider-side compaction as a future adapter option; **D8** and **D11** are confirmed, not changed. No
other accepted decision is contradicted.

## 6. What this note could not establish

Measurement must happen inside Ion; each has a cheapest experiment. Start from the M2 baseline and use
externally checked outcomes, per [`ROADMAP.md` §11](../../ROADMAP.md).

| Question | Cheapest experiment |
|---|---|
| Which edit representation wins for Ion's models? | One model, 20–30 small tasks; {whole-file, anchored search/replace, apply_patch-style hunk} on pass rate, malformed-edit rate, tokens |
| Where does tool-result shaping stop helping? | File-read/search tool with windows {50, 100, 300, full} on a localization suite, same model/budget |
| Does compaction beat mechanical shortening? | Same suite at {none, shorten-old-tool-results, summarise, 0.5 trigger, 0.8 trigger}; pass rate, tokens, turns |
| Do project instruction files change outcomes? | A/B the suite with and without a realistic `AGENTS.md`; subtract token cost |
| Does delegation help coding at equal token budget? | Single vs 2 workers (fresh vs inherited) on decomposable and sequential task pairs, identical budget |
| Does deferred tool discovery help? | Fixed large tool catalogue; upfront schemas vs on-demand discovery on task success and startup tokens |
| Does an edit guardrail help Ion? | Same edit corpus with and without syntax-check rejection; count recovered vs aborted tasks |
| What trigger point and budget are safe? | R8 budget-exhaustion and compaction-safety cases; unknown usage never treated as zero |

Not resolvable by reading: Cursor tool-call/approval internals, Claude Code tool-execution internals,
and Codex's exact default truncation/compaction thresholds. Do not fill these with inference.

## 7. References

All accessed 2026-09-14 unless noted.

- **Codex CLI source** — `openai/codex` @ `99b3ab2131a8672089fd7d78da62483187ba1122`:
  `codex-rs/core/src/agents_md.rs` (discovery/merge); `core/src/safety.rs` and
  `prompts/templates/permissions/*` (approval/sandbox, per-segment evaluation);
  `core/src/tools/handlers/apply_patch_spec.rs` and `assets/tools/apply_patch.lark` (freeform patch);
  `utils/output-truncation/src/lib.rs` (middle truncation + notice); `prompts/templates/compact/*`
  (handoff summary); `app-server/README.md` (compact/realtime/auto_review); `core/gpt_5_2_prompt.md`.
  Used for §2.1, §4. (Older `apply_patch` context/shell details are `[3P]`, search snippet only.)
- **Claude Code docs** — https://code.claude.com/docs/en/context-window (startup context; what survives
  compaction), `/memory`, `/tools-reference` (Bash output limits; exact-match Edit; subagents;
  background commands), `/permissions`, `/security`, `/agent-sdk/agent-loop` (stop; max_turns/budget),
  `/subagents`. Used for §2.2, §4.
- **Aider docs** — `Aider-AI/aider` @ `5dc9490bb35f9729ef2c95d00a19ccd30c26339c`:
  `aider/website/docs/more/edit-formats.md`, `repomap.md`, `usage/lint-test.md`, `git.md`,
  `usage/modes.md`; plus https://aider.chat/2023/12/21/unified-diffs.html (udiff vs SEARCH/REPLACE).
  Used for §2.3, §3.4.
- **SWE-agent** — Yang et al., arXiv 2405.15793 (v1 2024-05-06; NeurIPS 2024) and
  https://swe-agent.com ACI docs. Used for §2.4, §3.1, §3.4.
- **OpenHands** — platform paper arXiv 2407.16741; SDK docs
  https://docs.openhands.dev/sdk/arch/agent and `/sdk/guides/context-condenser`; vendor blogs
  2025-04-09 (condenser) and 2025-04-17 (critic/inference-time scaling); `str_replace_editor` schema
  quoted in OpenHands/OpenHands issue #4912. Used for §2.5, §3.3, §3.5.
- **Gemini CLI source/docs** — `google-gemini/gemini-cli` @
  `9c1b0a610534d6f8120964cf2672c07807d8fc90`: `docs/tools/file-system.md`, `docs/tools/shell.md`,
  `docs/reference/tools.md`, `docs/reference/commands.md`, `docs/reference/configuration.md`
  (`model.compressionThreshold`), `docs/cli/gemini-md.md`, `docs/cli/checkpointing.md`. Used for §2.6.
- **Cursor docs/blog** — https://cursor.com/docs/agent/overview, `/docs/rules`,
  `/docs/agent/chat/summarization` (condensation), `/docs/agent/prompting` (context tray),
  https://cursor.com/blog/self-summarization (Composer self-summarization). Used for §2.7, §3.3.
- **Same Model, Different Harness** — Lewis, arXiv 2608.26218 (2026-08-26). Used for §3.2.
- **Agentless** — Xia et al., arXiv 2407.01489 (v2 2024-10-29). Used for §3.1.
- **Anthropic multi-agent research system** —
  https://www.anthropic.com/engineering/multi-agent-research-system (2025-06-13). Used for §3.6.
- **Prior Ion reviews** — [`../research.md`](../research.md) (evidence rules, R8/R11, effectiveness
  list); [`agent-topology-context-2026-09-12.md`](agent-topology-context-2026-09-12.md) (CooperBench,
  Google scaling, *Less Context*); [`pico-core-and-ai-boundaries-2026-09-12.md`](pico-core-and-ai-boundaries-2026-09-12.md)
  (Pico2 gating, `ion-ai` boundary). Cited, not duplicated.
- **ICLR-2026 submission under review** — openreview `a84aea2b55f0d8862edb63354e1209776d2592a4`,
  Aider edit-format ablation. Cited cautiously as a conflicting preprint (§3.4).
