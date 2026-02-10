# System Prompt Effectiveness Analysis for ion

**Date:** 2026-02-10
**Scope:** Ion's DEFAULT_SYSTEM_PROMPT (~860 tokens, 6 sections)
**Method:** Cross-reference against competitor prompts, Anthropic's prompting docs, Replit's decision-time guidance research, pi-mono's minimalist approach, and practitioner reports

---

## 2. System Prompt Effectiveness Analysis

### Executive Summary

Ion's 860-token system prompt sits in the efficient middle ground between pi-mono (<1,000 tokens including tool defs) and Claude Code (~24,000 tokens across 133 conditional segments). Analysis suggests **3 of 6 sections encode genuinely necessary behavior**, 2 could be shortened substantially, and 1 is largely redundant with frontier model defaults. The most impactful instructions are those counteracting known model failure modes (early stopping, tool misrouting) rather than describing ideal behavior models already exhibit.

### Methodology

Evidence sources, ranked by reliability:

1. **Anthropic's official Claude 4.6 prompting docs** (platform.claude.com) -- first-party guidance on what models do/don't need
2. **Replit's Decision-Time Guidance** (Jan 2026) -- production data on static prompt limitations at scale
3. **Pi-mono benchmarks** (Terminal-Bench 2.0) -- empirical proof that minimal prompts work
4. **Codex CLI per-model prompts** -- GPT-5 Codex gets 68 lines vs 275 for base models, confirming smarter models need fewer instructions
5. **Practitioner consensus** (Addy Osmani, Mario Zechner) -- real-world workflow observations

---

### Section 1: Identity + Role (Lines 1-4)

```
You are ion, a fast terminal coding agent. You help users with software engineering tasks:
reading, editing, and creating files, running commands, and searching codebases.
Be concise and direct. Prioritize action over explanation.
```

**Tokens:** ~45
**Necessity: HIGH**

**Assessment:** This is the most token-efficient section and one of the most important. Identity framing is universally present across all coding agents, from pi-mono's one-sentence role to Claude Code's multi-paragraph identity. The "Be concise and direct" instruction is particularly valuable because Anthropic's own docs confirm Claude 4.6 naturally trends toward efficiency but can be steered either way.

**Evidence:**

- Anthropic docs: "Claude's latest models have a more concise and natural communication style" -- the model trends concise by default, but explicit instruction locks it in.
- Pi-mono uses a one-sentence equivalent. Still works.
- Claude Code uses a longer identity but the core function is the same: name + role + style.

**Verdict:** KEEP as-is. 45 tokens is already near-minimal. The "Prioritize action over explanation" framing is load-bearing -- without it, models default to explaining what they plan to do rather than doing it.

---

### Section 2: Core Principles (Lines 6-17)

```
## Core Principles
- Read code before modifying it. Understand context before making changes.
- Respect existing conventions...
- Make minimal, focused changes...
- Fix root causes, not symptoms...
- Write clean, idiomatic code...
- When deleting or moving code, remove it completely...
- Comments for non-obvious context only...
- Suggest nearby improvements worth considering, but don't make unrequested changes.
- If something seems wrong, stop and verify...
- Add error handling for real failure cases only...
```

**Tokens:** ~165
**Necessity: MIXED -- some lines critical, others redundant**

**Line-by-line assessment:**

| Instruction                                | Necessity | Rationale                                                                                                                                            |
| ------------------------------------------ | --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| Read code before modifying                 | CRITICAL  | Models skip reads without this. Pi-mono includes it. Claude Code says "NEVER propose changes to code you haven't read." Universal across all agents. |
| Respect existing conventions               | LOW       | Frontier models do this by default after RLHF. Pi-mono omits it.                                                                                     |
| Minimal, focused changes                   | MEDIUM    | Anthropic docs explicitly address "overeagerness" and provide a sample prompt to counter it. Claude 4.6 overengineers without guidance.              |
| Fix root causes                            | LOW       | Models attempt this naturally. No evidence prompting changes behavior.                                                                               |
| Write clean, idiomatic code                | LOW       | Redundant with RLHF training. Pi-mono omits it. No ablation evidence it matters.                                                                     |
| Remove code completely, no shims           | MEDIUM    | This is a style preference, not a natural model behavior. Without it, models leave `// deprecated` comments. Worth keeping as a brief preference.    |
| Comments for non-obvious context only      | MEDIUM    | Anthropic docs specifically call out that models add unnecessary docstrings/comments. Claude Code includes nearly identical wording.                 |
| Suggest but don't make unrequested changes | MEDIUM    | Counteracts Claude 4.6's "strong predilection" for proactive action (per Anthropic docs).                                                            |
| Stop and verify when something seems wrong | LOW       | Models do this variably. The real fix is environment-based (Replit's finding).                                                                       |
| Error handling for real cases only         | MEDIUM    | Anthropic docs: "Don't add error handling, fallbacks, or validation for scenarios that can't happen." This is a known overengineering pattern.       |

**Evidence for "read before edit":**

- Mario Zechner (pi-mono) includes this as one of his few instructions: "read before editing."
- Claude Code: "NEVER edit a file without reading it first."
- Codex CLI: implicit in `apply_patch` workflow (requires file context).
- Every agent enforces this. It is the single most universal instruction in coding agent prompts.
- Without it, models attempt edits based on assumed file contents and fail. This is one of the few instructions with clear behavioral evidence of being load-bearing.

**Recommendation:** SHORTEN to ~100 tokens. Keep: read-before-edit, minimal changes, no unnecessary comments/error-handling, remove code completely. Cut: respect conventions, fix root causes, write clean code (model defaults).

**Proposed reduction:**

```
## Core Principles
- ALWAYS read code before modifying it.
- Make minimal, focused changes. Don't add features or refactoring beyond what was asked.
- When deleting or moving code, remove it completely. No `// removed` or compatibility shims.
- Comments for non-obvious context only. Don't add docstrings to code you didn't change.
- Add error handling for real failure cases only. Don't handle impossible scenarios.
- Suggest nearby improvements worth considering, but don't make unrequested changes.
```

---

### Section 3: Task Execution (Lines 19-37)

```
## Task Execution
- Keep going until the task is fully resolved...
- Only yield to the user when you are confident the task is complete...
- Before tool calls, send a brief status message...
- Break complex tasks into logical steps...
- After making changes, verify your work...
- When something seems wrong or unexpected, stop and investigate...
- Do not guess or fabricate information...
- For new projects or greenfield tasks, be creative and ambitious...
```

**Tokens:** ~250
**Necessity: HIGH -- this is the highest-value section**

**The "keep going" problem is real and well-documented:**

This is the single most impactful section. Without explicit persistence instructions, LLMs default to stopping early -- after a single tool call or partial answer. This is not a model capability issue but a training artifact: models are RLHF-trained on short conversational turns, not multi-step agentic workflows.

**Evidence:**

- Anthropic's own prompting docs (Feb 2026) include verbatim examples of persistence instructions: "do not stop tasks early due to token budget concerns... Always be as persistent and autonomous as possible and complete tasks fully."
- Replit's Decision-Time Guidance (Jan 2026): "static prompt-based rules often fail to generalize" for long trajectories. They moved to environment-based feedback because prompts alone couldn't sustain agent persistence. This suggests the system prompt gets the model started, but isn't sufficient alone for very long tasks.
- Claude Code's system prompt includes nearly identical wording.
- Codex CLI's base prompt dedicates 20 lines to task execution persistence.
- The only agent that omits this is pi-mono, but pi-mono's author notes that "models are still poor at finding all the context needed" -- suggesting the minimal approach accepts some task-completion failure.

**The "status message before tool calls" instruction:**

- Present in Claude Code ("brief status updates before tool calls").
- Present in Codex CLI ("preamble messages" section with 8 examples, 20 lines).
- Anthropic docs confirm this needs explicit instruction: "Claude's latest models tend toward efficiency and may skip verbal summaries after tool calls, jumping directly to the next action."
- This instruction is load-bearing for UX. Without it, the TUI shows tool calls with no explanation, which is disorienting.

**The "verify your work" instruction:**

- Universal across agents. Replit built an entire self-testing infrastructure around this principle.
- Models frequently claim success without running tests. Explicit instruction improves but doesn't guarantee verification behavior.

**The "don't guess or fabricate" instruction:**

- Anthropic docs provide a specific prompt for this: "Never speculate about code you have not opened."
- Known failure mode. Worth keeping.

**Recommendation:** KEEP, minor tightening possible. This section earns its 250 tokens. The "greenfield vs existing codebase" line (last bullet) is the only candidate for removal -- it's a nuance most models handle without instruction.

---

### Section 4: Tool Usage (Lines 39-56)

```
## Tool Usage
Prefer specialized tools over bash equivalents:
- Use `read` to examine files, not `bash cat` or `bash head`.
- Use `grep` and `glob` to search, not `bash grep` or `bash find`.
- Use `edit` for precise changes to existing files, `write` for new files.
- Use `bash` for builds, tests, git operations, package managers, and system commands.

Critical rules:
- NEVER edit a file without reading it first...
- Run independent tool calls in parallel when possible...
- No interactive shell commands...
- Use the `directory` parameter in bash instead of `cd && cmd`.
- When searching for text, prefer `grep`...
- Don't re-read files after editing them...
- For long shell output, focus on relevant portions...
```

**Tokens:** ~215
**Necessity: MIXED -- tool routing is redundant, operational rules are valuable**

**Do frontier models need tool routing guidance?**

This is the key question. The answer is nuanced:

- **Anthropic docs (Claude 4.6):** "If your prompts were designed to reduce undertriggering on tools or skills, these models may now overtrigger. The fix is to dial back any aggressive language." This means Claude 4.6 triggers tools appropriately by default. Routing guidance is less necessary than with older models.
- **Pi-mono:** Uses only 4 tools (read/write/edit/bash) with no routing instructions. The model figures it out. But pi-mono also doesn't have specialized grep/glob tools -- bash handles everything.
- **Claude Code:** Still includes a full tool preference hierarchy (Read over cat, Grep over grep, etc.). But Claude Code supports 18+ tools, making routing more ambiguous.
- **Codex CLI (GPT-5 Codex):** The 68-line compact prompt includes minimal tool routing. The 275-line base prompt includes more. Confirming: smarter models need less guidance.

**The tool routing section (first 4 bullets) is semi-redundant** for frontier models with 6 tools. The model will naturally use `read` over `bash cat` because `read` is described as a file-reading tool. However, the grep/glob vs bash distinction is less obvious and does benefit from guidance.

**The operational rules (last 7 bullets) are high-value:**

- "NEVER edit without reading" -- already discussed; critical.
- "Parallel tool calls" -- Anthropic docs confirm this is promptable and beneficial: their sample prompt for parallel tool calling closely mirrors ion's instruction. Without it, models serialize unnecessarily.
- "No interactive commands" -- this prevents hard failures (hung processes). Essential.
- "directory parameter instead of cd" -- prevents stateful shell bugs. Essential.
- "Don't re-read after editing" -- saves tokens. Minor optimization.

**Recommendation:** SHORTEN the routing section, keep operational rules. The routing section could be a single line: "Prefer specialized tools (read, edit, grep, glob) over bash equivalents." The operational rules are high-value and should stay.

---

### Section 5: Output (Lines 58-64)

```
## Output
- Concise by default. Elaborate when the task requires it.
- Use markdown: code blocks with language tags, `backticks` for paths and identifiers.
- Reference files with line numbers: `src/main.rs:42`
- Brief status updates before tool calls to show progress.
- No ANSI escape codes in text output.
```

**Tokens:** ~65
**Necessity: MEDIUM-LOW**

**Assessment:**

- "Concise by default" -- already in the identity section ("Be concise and direct"). Redundant.
- "Use markdown" -- models default to markdown output. Anthropic docs don't mention needing to instruct this.
- "Reference files with line numbers" -- useful UX convention but not a natural model failure. Models sometimes do this, sometimes don't.
- "Status updates before tool calls" -- already in Task Execution section. Redundant.
- "No ANSI escape codes" -- essential for TUI rendering. Without this, models occasionally produce raw escape sequences. Load-bearing.

**Evidence:**

- Pi-mono: no output formatting instructions at all.
- Claude Code: extensive output formatting (markdown, no emoji, monospace).
- Codex CLI: 80 lines(!) of formatting rules in the base prompt, condensed to almost nothing in GPT-5 Codex prompt.

**Recommendation:** SHORTEN to 2 lines. Keep "No ANSI escape codes" (prevents rendering bugs). Keep file reference format (useful convention). Cut the rest as redundant with identity and task execution.

---

### Section 6: Safety (Lines 66-72)

```
## Safety
- Never force push to main/master without explicit request.
- Never skip git hooks or amend commits unless asked.
- Don't commit credentials, secrets, or .env files.
- Explain destructive commands before executing them.
- Respect AGENTS.md instructions from the project and user.
```

**Tokens:** ~55
**Necessity: HIGH**

**Assessment:**

Safety instructions are universally present across all coding agents, and for good reason:

- **Anthropic docs (Claude 4.6):** Explicitly warn that "Without guidance, Claude Opus 4.6 may take actions that are difficult to reverse or affect shared systems, such as deleting files, force-pushing, or posting to external services." They provide a sample safety prompt nearly identical to ion's.
- **Claude Code:** Massive safety section (git safety protocol, destructive action confirmation, secret detection).
- **Codex CLI:** Three-tier safety levels (Read Only, Auto, Full Access).
- **Pi-mono:** No safety instructions (full YOLO mode). But pi-mono targets power users who accept the risk.

The "Respect AGENTS.md instructions" line is important for the trust hierarchy -- it tells the model to treat project instructions as authoritative.

**Evidence for necessity:**

- Scheming research (Hopman et al., 2025) shows prompt factors influence agent behavior in safety-critical scenarios.
- Anthropic's own guidance recommends explicit safety guardrails even for their most capable models.
- Without force-push protection, models will `git push --force` when they encounter merge conflicts.

**Recommendation:** KEEP as-is. 55 tokens for safety is already minimal. Every line has a clear failure mode it prevents.

---

### Recommendation Summary

| Section            | Tokens | Verdict             | Rationale                                                                                                                                                     |
| ------------------ | ------ | ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1. Identity + Role | ~45    | **KEEP**            | Near-minimal. "Action over explanation" is load-bearing.                                                                                                      |
| 2. Core Principles | ~165   | **SHORTEN to ~100** | Cut redundant items (respect conventions, fix root causes, write clean code). Keep read-before-edit, minimal changes, no unnecessary comments/error-handling. |
| 3. Task Execution  | ~250   | **KEEP**            | Highest-value section. Persistence, verification, and status messages all counteract documented failure modes. Consider cutting greenfield line.              |
| 4. Tool Usage      | ~215   | **SHORTEN to ~150** | Condense routing to one line. Keep all operational rules (parallel calls, no interactive, directory param).                                                   |
| 5. Output          | ~65    | **SHORTEN to ~30**  | Remove redundancies with identity and task execution. Keep ANSI prohibition and file reference format.                                                        |
| 6. Safety          | ~55    | **KEEP**            | Already minimal. Every line prevents a documented failure mode.                                                                                               |

**Current total:** ~860 tokens
**Projected after cuts:** ~630 tokens (~27% reduction)

---

### Key Findings

**1. The persistence gap is real and system-prompt-solvable (partially).**

"Keep going until fully resolved" is the single most important instruction in any coding agent prompt. Every major agent includes it. Replit's research shows system prompts alone are insufficient for very long trajectories (prompts degrade over context), but they are necessary to establish the baseline behavior. For ion's typical session lengths, the system prompt is likely sufficient.

**2. "Read before edit" is universal and load-bearing.**

This is the one instruction that appears in literally every coding agent, including pi-mono's minimal prompt. Without it, models attempt blind edits based on assumed file contents. It is not redundant with model training -- the failure mode is well-documented.

**3. Frontier models (Claude 4.6, GPT-5) need less tool routing than older models.**

Anthropic's own docs warn against over-prompting tool use for Claude 4.6. The model triggers tools appropriately by default. Ion's explicit routing (use read not cat, use grep not bash grep) is somewhat redundant but low-cost. With only 6 tools, routing ambiguity is low.

**4. Overengineering prevention is model-specific and necessary for Claude.**

Anthropic documents "overeagerness" as a specific Claude 4.6 behavior that requires explicit prompting to control. The "minimal, focused changes" and "don't add unnecessary error handling" instructions directly counteract this. These lines earn their tokens specifically for Claude models. They may be less necessary for GPT-5 or Gemini.

**5. Static prompts have diminishing returns past a point.**

Replit found that "adding constraints increases cost and priority ambiguity, and often forces the model to reason over rules that don't matter for the current decision." This validates ion's relatively brief prompt over Claude Code's exhaustive approach. The question isn't whether a 24k-token prompt is better than an 860-token prompt -- it's whether the marginal instructions in those extra 23k tokens are worth the context cost.

**6. Per-model prompts are the future but not urgent.**

Codex CLI maintains per-model prompt variants (68 lines for GPT-5 Codex vs 275 for base). Ion could benefit from this eventually, but with its current multi-provider architecture, the single prompt is a reasonable simplification.

---

### What Pi-Mono Omits and Gets Right

Pi-mono omits:

- Output formatting rules (concise, markdown, file references)
- Overengineering prevention (minimal changes, no unnecessary comments)
- Verification instructions (run tests, check builds)
- Safety guardrails (force push, secrets, destructive commands)
- Detailed tool routing
- Status message requirements

Pi-mono still works because:

1. **Frontier models** encode most coding-agent behavior through RLHF training
2. **4 tools** makes routing trivial (no grep/glob vs bash ambiguity)
3. **YOLO mode** eliminates safety instruction needs
4. **Power users** compensate for missing guardrails
5. **External state** (PLAN.md, TODO.md) replaces model-side task tracking

But pi-mono also accepts certain failure modes that ion's prompt prevents: overengineering, missing verifications, unnecessary verbose output, and safety incidents. The tradeoff is token efficiency vs behavioral reliability.

---

### Environment vs Prompt: Replit's Findings

Replit's Decision-Time Guidance (January 2026) is the most relevant research on system prompt limitations:

> "Static prompt-based rules often fail to generalize -- or worse, pollute the context as they scale."

Key findings:

- **Learned priors override written rules:** Models fall back to pre-training behaviors when rules are verbose, ambiguous, or conflicting.
- **Instruction-following degrades as context grows:** Primacy/recency bias means mid-context rules lose influence.
- **More rules have diminishing returns:** Additional constraints increase cost and priority ambiguity.

Replit's solution: inject contextual guidance at decision time based on environment state (build failures, test results) rather than front-loading all rules into the system prompt.

**Implication for ion:** The system prompt should establish baseline behavior (what pi-mono proves is sufficient), and critical behavioral corrections (what Anthropic docs identify as necessary). Everything beyond that has diminishing returns. Ion's current 860 tokens is already in the efficient zone. The proposed reduction to ~630 tokens moves it closer to optimal without losing behavioral reliability.

---

### Sources

- [Anthropic Claude 4.6 Prompting Best Practices](https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-4-best-practices)
- [Replit Decision-Time Guidance](https://blog.replit.com/decision-time-guidance) (Jan 2026)
- [Mario Zechner: What I Learned Building a Minimal Coding Agent](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/)
- [Codex CLI System Prompt Architecture](https://github.com/openai/codex) -- per-model prompt variants
- [Addy Osmani: My LLM Coding Workflow into 2026](https://addyosmani.com/blog/ai-coding-workflow/)
- [Mu et al.: A Closer Look at System Prompt Robustness](https://arxiv.org/pdf/2502.12197) (Feb 2025)
- Piebald-AI/claude-code-system-prompts -- Claude Code prompt extraction
- ion prior research: ai/research/coding-agents-state-2026-02.md, ai/research/codex-cli-system-prompt-tools-2026.md
