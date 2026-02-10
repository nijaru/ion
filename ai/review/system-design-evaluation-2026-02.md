# Ion System Design Evaluation

**Date**: 2026-02-10
**Scope**: Five design questions evaluated with empirical evidence
**Method**: Parallel research across benchmarks, practitioner reports, academic papers, and source code analysis
**Detailed research**: `ai/research/*-2026-02.md` (5 files)

---

## Executive Summary

| Topic              | Verdict      | Action                                        | Priority            |
| ------------------ | ------------ | --------------------------------------------- | ------------------- |
| Designer/Plan Mode | **Remove**   | Delete 134 LOC + integrations across 5 files  | P1 (simplification) |
| System Prompt      | **Trim 27%** | Cut ~230 tokens (860 -> ~630) from 3 sections | P3                  |
| Failure Tracking   | **Build**    | ~300 LOC new module, in-memory per session    | P2                  |
| LSP Integration    | **Defer**    | Monitor; invest when eval data improves       | P4                  |
| Feature Gaps       | **Ship P1s** | Sandbox, session resume, headless mode        | P1                  |

**Strategic direction**: Move toward Pi-mono's efficiency, not Claude Code's feature maximalism. Terminal-Bench 2.0 shows minimal scaffolds outperform complex agents. Ion's identity: fast, efficient, multi-provider.

---

## 1. Designer/Plan Mode: Remove

**Full analysis**: `ai/research/designer-plan-mode-evaluation-2026-02.md`

### Dead Code

| Element                     | Status                           |
| --------------------------- | -------------------------------- |
| `Plan::mark_task()`         | Defined, never called            |
| `recommended_fresh_context` | Deserialized, never read         |
| `PlannedTask::dependencies` | Displayed, never enforced        |
| `TaskStatus` transitions    | All tasks stay `Pending` forever |

The plan is generated once and frozen. FOCUS always points at task-1. The VERIFICATION directive has no mechanism to advance state.

### Cost

- Full non-streaming API call on every first message >100 chars
- Blocking latency before first streamed output
- ~100-300 tokens of plan text in every subsequent system prompt
- 134 LOC in designer.rs + integration across 4 more files
- Fragile JSON extraction regex (`(?s)\{.*?\}`)

### Industry Consensus

Amp removed plan mode ("just a prompt"). Pi-mono never built it. Claude Code's version is contested ("mostly a custom prompt"). Codex makes it user-invoked (`/plan`), not automatic. No major agent ships ion's "generate once, inject forever, never update" approach.

### Evidence

No published study supports static plan injection. Research supporting planning (Verdent, Refact.ai) is about **dynamic, in-loop** planning. Agentless achieves 32% on SWE-bench Lite with zero autonomous planning at $0.70/task.

### Action

- Delete `src/agent/designer.rs`
- Remove `active_plan` from Agent, plan template from `context.rs`, trigger logic from `mod.rs`
- Remove `PlanGenerated` from `AgentEvent`, plan display from `message_list.rs`
- Consider: add a "plan" skill for explicit user invocation

---

## 2. System Prompt: Trim 27%

**Full analysis**: `ai/research/system-prompt-effectiveness-2026-02.md`

### Section-by-Section Verdict

| Section         | Tokens | Verdict             | Rationale                                                                                                                                                         |
| --------------- | ------ | ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Identity + Role | ~45    | **KEEP**            | "Prioritize action over explanation" is load-bearing                                                                                                              |
| Core Principles | ~165   | **SHORTEN to ~100** | Cut "respect conventions", "fix root causes", "write clean code" (model defaults). Keep read-before-edit, minimal changes, no unnecessary comments/error-handling |
| Task Execution  | ~250   | **KEEP**            | Highest-value section. "Keep going" counteracts documented early-stopping. "Status before tool calls" confirmed necessary by Anthropic                            |
| Tool Usage      | ~215   | **SHORTEN to ~150** | Condense routing to one line. Keep operational rules (parallel calls, no interactive, directory param)                                                            |
| Output          | ~65    | **SHORTEN to ~30**  | Redundant with identity + task execution. Keep only "No ANSI escape codes" and file reference format                                                              |
| Safety          | ~55    | **KEEP**            | Anthropic: "Without guidance, Claude Opus 4.6 may take actions that are difficult to reverse." Every line prevents a documented failure mode                      |

**Current**: ~860 tokens. **Target**: ~630 tokens.

### Key Findings

1. **"Keep going until fully resolved"** is the single most important instruction. Every agent includes it. Without it, models stop after one tool call. Anthropic's prompting docs include verbatim persistence language.

2. **"Read before edit"** is universal and load-bearing. Present in every agent including pi-mono's minimal prompt. Models attempt blind edits without it.

3. **Frontier models need less tool routing** than older models. Anthropic warns against over-prompting tool use for Claude 4.6. With 6 tools, routing is mostly self-evident.

4. **Overengineering prevention** is Claude-specific and necessary. Anthropic documents "overeagerness" as a behavior requiring explicit control.

5. **Static prompts have diminishing returns** (Replit, Jan 2026). Ion's ~860 tokens is already in the efficient zone. The reduction to ~630 moves closer to optimal.

### Proposed Cuts

**Core Principles** -- remove 4 lines:

- "Respect existing conventions" (model default)
- "Fix root causes, not symptoms" (model default)
- "Write clean, idiomatic code" (model default)
- "If something seems wrong, stop and verify" (duplicate of Task Execution)

**Tool Usage** -- condense routing:

- Replace 4 routing lines with: "Prefer specialized tools (read, edit, grep, glob) over bash equivalents."
- Keep all 7 operational rules unchanged

**Output** -- reduce to 2 lines:

- Keep: "No ANSI escape codes in text output."
- Keep: "Reference files with line numbers: `src/main.rs:42`"
- Cut: "Concise by default" (in identity), "Use markdown" (model default), "Status updates" (in task execution)

---

## 3. Failure Tracking: Build

**Full analysis**: `ai/research/failure-tracking-design-2026-02.md`

### The Gap

Tool errors become `ToolResult { is_error: true }` in conversation history. During compaction, error details are lost or compressed to narrative summaries. No coding agent currently implements structured failure tracking across compaction.

### Evidence

| Paper                            | Finding                                                                                             |
| -------------------------------- | --------------------------------------------------------------------------------------------------- |
| **Recovery-Bench** (Letta, 2025) | 57% accuracy drop on recovery tasks. Full error histories _hurt_ -- brief structured summaries help |
| **PALADIN** (2025)               | Recovery rate: 32% -> 90% with failure taxonomy + exemplars                                         |
| **SABER** (ICLR 2026)            | Each mutating action error reduces success odds by up to 96%                                        |
| Claude Code #13919, #7919        | Users report "repeated errors and massive productivity loss" after compaction                       |

### Design

```rust
pub struct FailureTracker {
    records: Vec<FailureRecord>,  // max 10
    turn_counter: usize,
}

pub enum FailureCategory {
    EditMismatch,    // edit old_string didn't match
    FileNotFound,    // wrong path
    BuildFailure,    // compiler errors
    TestFailure,     // test failures
    ToolError,       // timeout, permission, etc.
    WrongApproach,   // unexpected output
}
```

**Storage**: In-memory, per session. Not persisted -- failure context is session-scoped.

**Injection**: Minijinja template section `## Recent Failures` in system prompt, gated to only appear **after first compaction**. Before compaction, errors are visible in conversation history.

**Token budget**: ~500 tokens max (0.25% of 200k context). FIFO eviction with category dedup.

### Implementation

| File                         | Change                                   | LOC      |
| ---------------------------- | ---------------------------------------- | -------- |
| `src/agent/failure.rs` (new) | Data structures, tracker, classification | ~120     |
| `src/agent/tools.rs`         | Classify errors on `is_error: true`      | ~40      |
| `src/agent/context.rs`       | Template section + render integration    | ~25      |
| `src/agent/mod.rs`           | Wire tracker into Agent                  | ~15      |
| Tests                        | Classification, eviction, template       | ~100     |
| **Total**                    |                                          | **~300** |

### Risk Mitigation

- **Token overhead**: Post-compaction-only gate eliminates redundancy during short sessions
- **Stale data**: FIFO eviction + category dedup naturally cycles records
- **Over-cautious behavior**: "Avoid repeating these patterns" framing, not "be extra careful about everything"
- **False classification**: Only classify `is_error: true` results; conservative heuristics

---

## 4. LSP Integration: Defer (P4)

**Full analysis**: `ai/research/lsp-cost-benefit-2026-02.md`

### The Controlled Evidence

Nuanced.dev ran the **only rigorous evaluation** of LSP impact on coding agents:

- 720 runs, 2 models, 10 SWE-bench tasks
- Finding: "External code intelligence tools did not deliver consistently better outcomes"
- ">50% reductions in some runs, ~50% improvements in others, under identical conditions"

This contradicts practitioner enthusiasm ("single biggest productivity gain") with controlled measurement.

### grep + bash Covers 80-85%

LSP's advantages (precise cross-file references, trait resolution, type-aware rename) matter most in large, complex codebases with deep type hierarchies. For most coding tasks, `grep` and `cargo check` suffice.

### Resource Costs

| Server        | Medium Project | Large Project |
| ------------- | -------------- | ------------- |
| rust-analyzer | 1.5-3GB        | 5-6GB         |
| tsserver      | 500MB-1.5GB    | 1.5-3GB       |
| gopls         | 400MB-1GB      | 1-3GB         |

Plus: staleness after edits (OpenCode #5899), memory leaks in long sessions (rust-analyzer #20949), cold start latency (10-60s for rust-analyzer), no mature Rust client crate.

### Recommendation

**P4 -- Defer**. Invest in memory system instead. Revisit when:

1. Models are trained to prefer LSP over grep (currently they default to grep even when LSP is available)
2. Evaluation data shows consistent improvement
3. A mature Rust LSP client crate exists

**Phase 1 alternative**: Parse `cargo check --message-format=json` for structured diagnostics. Captures ~60% of LSP value for near-zero implementation cost.

---

## 5. Feature Gap Analysis: Ship P1s

**Full analysis**: `ai/research/feature-gap-analysis-2026-02.md`

### Critical Finding: Model Quality >> Feature Count

Terminal-Bench 2.0: "Simple Codex" (minimal scaffold) at 75.1% beats "Droid" (full agent) at 69.9%. Terminus 2 (just a tmux session) scores 64.7%. The same scaffold scores 75.1% with GPT-5.3-Codex and 3.1% with a weak model.

Ion's multi-provider support -- letting users pick the best available model -- is its most important architectural decision.

### Ion's Differentiators

| Differentiator        | Competitive Position                                            |
| --------------------- | --------------------------------------------------------------- |
| Multi-provider (7)    | Unique vs CC/Codex (single-provider)                            |
| Rust native           | Single binary, fast startup, low memory. Shared only with Codex |
| 3-tier compaction     | More sophisticated than competitors                             |
| Provider-side caching | Stable system prompt prefix for cache hits                      |
| Web tools built-in    | CC uses MCP, Pi uses bash                                       |
| Permission simplicity | Read/Write modes, no approval prompts                           |

### Priority Matrix

**P1 -- Critical for competitive parity:**

| Feature                          | Effort   | Impact                            |
| -------------------------------- | -------- | --------------------------------- |
| OS sandbox (Seatbelt + Landlock) | 2-3 days | 84% fewer interruptions (CC data) |
| Session resume (`/resume`)       | 1 day    | Every competitor has this         |
| CLI headless mode (`ion run`)    | 3-5 days | CI/CD, scripting, automation      |

**P2 -- Meaningful improvement:**

| Feature                 | Effort   | Impact                                             |
| ----------------------- | -------- | -------------------------------------------------- |
| Wire hooks to config    | 1 day    | Framework 80% complete; enables cargo fmt, linting |
| Git safety guards       | 0.5 day  | Block `--force`, `reset --hard`                    |
| Failure tracking (R3)   | 2-3 days | ~300 LOC, no competitor does this                  |
| Mid-turn steering UX    | 1 day    | message_queue exists, needs UX                     |
| Parallel tool execution | 2-3 days | Direct speedup                                     |

**Simplify:**

| Feature            | Action                        | Rationale                                              |
| ------------------ | ----------------------------- | ------------------------------------------------------ |
| Designer/plan mode | Remove auto-trigger           | Half-implemented, costs API call, industry moving away |
| Skills system      | Ship 2-3 defaults or document | Infrastructure exists but no content                   |
| Hook system        | Wire or remove                | 426 LOC framework, not connected to config             |

**Defer (P3-P4):**

| Feature            | Condition                             |
| ------------------ | ------------------------------------- |
| LSP integration    | Wait for stronger eval data           |
| General sub-agents | Wait for user demand                  |
| Checkpoints/undo   | Wait for clean implementation pattern |
| Persistent memory  | Wait for stable OmenDB                |

### The Convergent Minimum (What Every Competitive Agent Needs)

```
Must have:  Agent loop, file tools, bash, search, AGENTS.md, sessions,
            compaction, streaming, markdown, sandbox
Should have: Multi-provider, skills, hooks, cost tracking, headless mode,
             parallel tools, git safety, thinking mode
Skip:       Built-in memory, MCP server mode, plugin marketplace,
            full-screen TUI, complex permissions, auto-plan mode
```

---

## Prioritized Action Plan

### This Sprint

| #   | Action                                   | Effort   | Type           |
| --- | ---------------------------------------- | -------- | -------------- |
| 1   | Remove designer auto-trigger + dead code | 0.5 day  | Simplification |
| 2   | Ship OS sandbox                          | 2-3 days | P1 feature     |
| 3   | Add `/resume` command                    | 1 day    | P1 feature     |
| 4   | Wire hooks to TOML config                | 1 day    | P2 feature     |
| 5   | Add git safety guards                    | 0.5 day  | P2 feature     |
| 6   | Trim system prompt (27%)                 | 0.5 day  | Optimization   |

### Next Sprint

| #   | Action                       | Effort   | Type                        |
| --- | ---------------------------- | -------- | --------------------------- |
| 7   | Ship `ion run` headless mode | 3-5 days | P1 feature                  |
| 8   | Build failure tracking       | 2-3 days | P2 feature (differentiator) |
| 9   | Parallel tool execution      | 2-3 days | P2 feature                  |
| 10  | Polish mid-turn steering UX  | 1 day    | P2 feature                  |
| 11  | Ship 2-3 default skills      | 1 day    | Activate existing infra     |

### Backlog

| #   | Action             | Trigger                                |
| --- | ------------------ | -------------------------------------- |
| 12  | LSP integration    | Eval data shows consistent improvement |
| 13  | General sub-agents | User demand                            |
| 14  | Persistent memory  | Stable Rust OmenDB                     |
| 15  | Checkpoints/undo   | Clean implementation pattern           |

---

## Sources

### Research Files (detailed analysis)

- `ai/research/designer-plan-mode-evaluation-2026-02.md`
- `ai/research/system-prompt-effectiveness-2026-02.md`
- `ai/research/failure-tracking-design-2026-02.md`
- `ai/research/lsp-cost-benefit-2026-02.md`
- `ai/research/feature-gap-analysis-2026-02.md`

### Key External Sources

- [Armin Ronacher: What Is Plan Mode?](https://lucumr.pocoo.org/2025/12/17/what-is-plan-mode/)
- [Anthropic Claude 4.6 Prompting Best Practices](https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-4-best-practices)
- [Replit Decision-Time Guidance](https://blog.replit.com/decision-time-guidance) (Jan 2026)
- [Recovery-Bench](https://www.letta.com/blog/recovery-bench) (Letta, Aug 2025)
- [PALADIN](https://arxiv.org/abs/2509.25238) -- Self-correcting agents
- [SABER](https://openreview.net/forum?id=JuwuBUnoJk) -- ICLR 2026
- [Nuanced.dev LSP Evaluation](https://www.nuanced.dev/blog/evaluating-lsp) -- 720 controlled runs
- [Verdent SWE-bench Report](https://www.verdent.ai/blog/swe-bench-verified-technical-report)
- [Agentless](https://arxiv.org/abs/2407.01489) -- Demystifying LLM-based agents
- [Amp Chronicle](https://ampcode.com/chronicle) -- Feature removal history
- Terminal-Bench 2.0 leaderboard (Feb 2026)
- ACTI Survey (271 developers, Jan 2026)
