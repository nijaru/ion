# Designer/Plan Mode A/B Evaluation

**Research Date**: 2026-02-10
**Scope**: Evaluate whether ion's designer/plan mode improves task completion, and whether to keep, modify, or remove it.

---

## 1. Designer/Plan Mode Evaluation

### 1.1 Dead Code Analysis

The designer module (`src/agent/designer.rs`, 134 lines) contains four pieces of dead or inert code:

| Element                     | Location          | Status                                                                                            |
| --------------------------- | ----------------- | ------------------------------------------------------------------------------------------------- |
| `Plan::mark_task()`         | designer.rs:45    | Defined, **never called** anywhere in codebase                                                    |
| `recommended_fresh_context` | designer.rs:34    | Field deserialized from LLM JSON, **never read** at runtime                                       |
| `PlannedTask::dependencies` | designer.rs:26    | Deserialized and displayed in TUI (message_list.rs:638), but **never enforced** for task ordering |
| `TaskStatus` enum variants  | designer.rs:14-19 | `InProgress`, `Completed`, `Failed` defined but tasks **never transition** from `Pending`         |

**Consequence**: The plan is generated once, injected into the system prompt, and never updated. All tasks remain `Pending` forever. The `current_task()` method always returns the first task because no task is ever marked `InProgress` or `Completed`. The FOCUS directive in the system prompt template always points at task-1, regardless of agent progress.

The system prompt template (`src/agent/context.rs:52-63`) renders plan status markers (`[x]`, `[>]`, `[!]`, `[ ]`) but since `mark_task` is never called, every task always renders as `[ ]` (Pending). The VERIFICATION instruction ("After each tool call, verify if the output matches the requirements of this task") is injected but the agent has no mechanism to report verification results or advance task state.

### 1.2 Cost-Benefit Analysis

**Costs:**

| Cost                 | Detail                                                                                                                                                                                                 |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Extra API call       | Full non-streaming `complete()` call with `temperature: 0.0` on every qualifying first message                                                                                                         |
| Token overhead       | System prompt (DESIGNER_SYSTEM: ~200 tokens) + full session history sent to the planning call, then plan text (~100-300 tokens) appended to every subsequent system prompt for the rest of the session |
| Latency              | Blocking call before the first streamed response. User sees no output until the plan completes.                                                                                                        |
| Code complexity      | 134 lines in designer.rs + integration in mod.rs (trigger logic) + context.rs (template rendering) + stream.rs (plan passing) + message_list.rs (TUI display)                                          |
| Fragile JSON parsing | Uses `r"(?s)\{.*?\}"` (non-greedy) regex to extract JSON from model output. This fails on nested objects or when the model includes explanation text containing braces.                                |

**Benefits:**

| Benefit                | Detail                                                                   |
| ---------------------- | ------------------------------------------------------------------------ |
| System prompt guidance | Plan title and task list visible to the model in every turn              |
| FOCUS directive        | Tells the model which task to work on (but always task-1, see dead code) |
| User visibility        | Plan displayed in TUI as a checklist                                     |

**Net assessment**: The plan provides a static, never-updating checklist in the system prompt. Since task state never advances, the "focus" benefit degrades to "always pointing at the first task." The user sees a plan once in the TUI but has no way to interact with it. The cost is a full extra API call on every long first message, guaranteed latency before first output, and ~200 lines of code across five files.

### 1.3 Industry Comparison

| Agent           | Plan Mode           | Status               | Rationale                                                                                                          |
| --------------- | ------------------- | -------------------- | ------------------------------------------------------------------------------------------------------------------ |
| **Amp**         | Removed             | Gone (Jan 2026)      | "Just a prompt." Replaced by user instruction: "Only plan, don't code."                                            |
| **Claude Code** | Built-in            | Active but contested | Armin Ronacher: "mostly a custom prompt to give it structure, and some system reminders and a handful of examples" |
| **Pi-mono**     | Never built         | N/A                  | Plans written to files for cross-session persistence. "If I don't need it, won't build it."                        |
| **OpenCode**    | Never built         | N/A                  | Follows pi-mono philosophy                                                                                         |
| **Codex**       | `/plan` command     | User-invoked         | Not automatic. User explicitly triggers it when they want planning.                                                |
| **Verdent**     | Todo list           | Active               | Runtime todo list that updates during execution. Agent reads/writes list continuously.                             |
| **Agentless**   | Structured pipeline | Active               | Fixed localization-repair-validation pipeline. No autonomous planning by the LLM.                                  |

**The trend**: The industry is moving in two directions, neither of which is ion's current approach.

1. **Remove it entirely** (Amp): Modern models plan implicitly. Users who want explicit planning can prompt for it.
2. **Make it dynamic** (Verdent): An updating todo list that the agent actively maintains, rather than a static injection.

No major agent ships a "generate once, inject into system prompt, never update" approach like ion's.

### 1.4 The "Just a Prompt" Argument

Armin Ronacher's [December 2025 analysis](https://lucumr.pocoo.org/2025/12/17/what-is-plan-mode/) demonstrated that Claude Code's plan mode amounts to:

- A markdown file written to a hidden folder
- A short system prompt injection telling the model to plan rather than act
- Tool restrictions enforced via prompt reinforcement, not actual capability removal

His conclusion: "I'm mostly a custom prompt to give it structure, and some system reminders and a handful of examples."

**Ion's implementation is weaker than Claude Code's plan mode**, and Claude Code's plan mode is already considered "just a prompt" by practitioners. Specifically:

- Claude Code's plan mode at least creates a persistent file the user can edit
- Claude Code's plan mode has an explicit enter/exit workflow with UI affordance
- Ion's plan is never visible to the user as an editable artifact
- Ion's plan state never updates -- it is literally a static string injected at generation time

If Claude Code's plan mode is "just a prompt," ion's is "just a prompt fragment that was generated by another prompt."

### 1.5 Research Evidence

No published study performs a clean "plan vs no-plan" ablation for coding agents. The closest evidence:

**Supporting planning (qualified):**

- Verdent's SWE-bench report states that structured todo lists "improve task resolution rates while reducing wasted tokens" when issue descriptions are vague. However, their ablation removing advanced tools (keeping only bash, read, write, edit) showed **little performance change**, suggesting the planning benefit is hard to isolate.
- SWE-EVO (Dec 2025) shows planning matters more for multi-file, multi-commit tasks -- GPT-5 achieves only 21% on SWE-EVO vs 65% on SWE-bench Verified. But this tests sustained reasoning, not static plan injection.
- Refact.ai's `deep_analysis()` tool provides structured reasoning at critical moments, but it is **invoked dynamically by the agent**, not pre-generated.

**Against static planning:**

- Agentless (2024-2025) achieves 32% on SWE-bench Lite with a fixed pipeline and no autonomous planning, at $0.70/task -- proving that structured pipelines can outperform complex planning agents.
- Qwen3 models show "similar performance with or without think mode," suggesting explicit planning at the model level may not help.
- Amp removed their TODO list feature in January 2026, citing that "with Opus 4.5, we found it's no longer needed" -- models track work without explicit lists.

**Key distinction**: The research consistently shows that **dynamic, in-loop planning** (where the agent maintains and updates a task list during execution) helps. **Static, pre-generated plans injected into system prompts** have no supporting evidence.

Ion's designer generates a static plan. It is not dynamic. The agent cannot update it.

### 1.6 Recommendation: Remove

**Verdict**: Remove the designer/plan mode entirely.

**Rationale:**

1. **It does not work as designed.** Task state never updates. Dependencies are never enforced. The "focus" always points at task-1. The feature is half-implemented.
2. **Static plans lack evidence.** No research supports injecting a pre-generated static plan into a system prompt. The research that supports planning (Verdent, Refact.ai) is about dynamic, runtime planning.
3. **Industry consensus.** Amp removed it. Claude Code's version is widely considered "just a prompt." Pi-mono never built it. The trend is toward either removing plan modes or making them fully dynamic.
4. **Cost is real, benefit is speculative.** An extra API call, guaranteed first-message latency, and ~200 lines of code across 5 files -- for a static text injection that the model may or may not attend to.
5. **Better alternatives exist.** Users who want planning can prompt for it: "Plan this task before implementing. Write the plan, then execute." This costs nothing in code complexity and lets the user see and modify the plan.

**Migration path:**

- Delete `src/agent/designer.rs`
- Remove `active_plan` from agent state, `Designer` integration from `mod.rs`, plan template from `context.rs`, plan rendering from `stream.rs` and `message_list.rs`
- Remove `PlanGenerated` variant from `AgentEvent`
- Consider: add a "plan" skill (`SKILL.md`) that users can invoke explicitly, replicating the "just ask" approach

**If the feature must be kept**, it should be rebuilt as a dynamic system: the agent should be able to mark tasks complete, the plan should update in the system prompt, and the user should be able to view and edit the plan. This is a significant investment for uncertain benefit.

---

## Sources

- [Armin Ronacher: What Actually Is Claude Code's Plan Mode?](https://lucumr.pocoo.org/2025/12/17/what-is-plan-mode/)
- [Amp Chronicle](https://ampcode.com/chronicle)
- [Amp Owner's Manual](https://ampcode.com/manual)
- [Verdent SWE-bench Verified Technical Report](https://www.verdent.ai/blog/swe-bench-verified-technical-report)
- [Agentless: Demystifying LLM-based Software Engineering Agents](https://arxiv.org/abs/2407.01489)
- [SWE-EVO: Benchmarking Coding Agents in Long-Horizon Software Evolution](https://arxiv.org/html/2512.18470v2)
- [Kimi-Dev: Agentless Training as Skill Prior for SWE-Agents](https://arxiv.org/html/2509.23045v3)
- [Refact.ai SOTA on SWE-bench Lite](https://refact.ai/blog/2025/sota-on-swe-bench-lite-open-source-refact-ai/)
- [Rethinking Coding Agent Benchmarks (Jarmak, Jan 2026)](https://medium.com/@steph.jarmak/rethinking-coding-agent-benchmarks-5cde3c696e4a)
- [In-the-Flow Agentic System Optimization for Effective Planning and Tool Use](https://arxiv.org/abs/2510.05592)
- ion source: `src/agent/designer.rs`, `src/agent/mod.rs`, `src/agent/context.rs`, `src/agent/stream.rs`, `src/tui/message_list.rs`
