# System Prompt Survey: Top Coding Agents

**Date:** 2026-02-14
**Scope:** Actual system prompts (leaked/published/open-source) for Claude Code, Gemini CLI, OpenCode, Codex CLI, and Amp
**Sources:** GitHub gists (chigkim, ksprashu, gregce), google-gemini/gemini-cli source, anomalyco/opencode source, wunderwuzzi23/scratch, x1xhlol/system-prompts-and-models-of-ai-tools

---

## Quick Comparison

| Agent                 | Est. Tokens                | Sections      | Identity                                               | Tone                                        | Autonomy                                | Task Tracking                    | Subagents                                 | Parallel Tool Calls                |
| --------------------- | -------------------------- | ------------- | ------------------------------------------------------ | ------------------------------------------- | --------------------------------------- | -------------------------------- | ----------------------------------------- | ---------------------------------- |
| Claude Code           | ~6,000+ (core)             | ~12           | "Claude Code, Anthropic's official CLI"                | Concise, no emoji, professional objectivity | High, ask when blocked                  | TodoWrite (mandatory)            | Yes (Explore, Plan, Bash, general)        | Yes, explicit policy               |
| Gemini CLI            | ~4,000+ (composed)         | ~10 (modular) | "Gemini CLI, interactive CLI agent"                    | Concise, <3 lines, no chitchat              | Research-Strategy-Execute lifecycle     | write_todos (optional)           | Yes (codebase_investigator)               | Yes, explicit                      |
| OpenCode              | ~3,500 (Anthropic variant) | ~8            | "OpenCode, the best coding agent on the planet"        | Concise, no emoji, no chitchat              | High, ask only when truly blocked       | TodoWrite (mandatory)            | Yes (Task, Explore)                       | Yes, explicit                      |
| Codex CLI             | ~4,500                     | ~10           | "coding agent running in the Codex CLI"                | Concise, direct, friendly                   | High, keep going until resolved         | update_plan                      | No (single agent)                         | Not explicit (uses apply_patch)    |
| Amp (Aug 2025)        | ~4,000                     | ~8            | "Amp, a powerful AI coding agent built by Sourcegraph" | Concise, <4 lines, no emoji, no flattery    | High, balance initiative with restraint | todo_write/todo_read (mandatory) | Yes (Task, Oracle, codebase_search_agent) | Yes, very detailed policy          |
| Amp (Jan 2026, GPT-5) | ~3,500                     | ~10           | Same identity                                          | Same + MINIMIZE REASONING                   | Higher, end-to-end completion           | Same                             | Same                                      | Detailed parallel execution policy |

---

## 1. Claude Code

**Source:** Leaked prompt, v2.1.x (Jan 2026, gist by chigkim). Runs on multiple models (not just Claude).

### Structure (in order)

1. **Identity** -- "You are Claude Code, Anthropic's official CLI for Claude"
2. **Security preamble** -- IMPORTANT blocks for authorized security testing, URL generation rules
3. **Feedback/help** -- /help command, issue reporting
4. **Tone and style** -- No emoji, concise for CLI, GFM markdown, no colons before tool calls
5. **Professional objectivity** -- Anti-sycophancy: "Avoid over-the-top validation", "respectful correction over false agreement"
6. **Planning without timelines** -- Never suggest time estimates
7. **Task Management** -- TodoWrite tool usage with detailed examples
8. **Asking questions** -- AskUserQuestion tool guidance, hook handling
9. **Doing tasks** -- Core workflow: read first, plan, avoid over-engineering, security awareness
10. **Tool usage policy** -- Parallel execution, Task tool for exploration, specialized tools over bash
11. **Code References** -- file_path:line_number pattern
12. **Environment** -- Working directory, platform, date, git status

### Key Behavioral Instructions

**Anti-sycophancy (unique and effective):**

> "Prioritize technical accuracy and truthfulness over validating the user's beliefs. Focus on facts and problem-solving, providing direct, objective technical info without any unnecessary superlatives, praise, or emotional validation."

**Over-engineering prevention (detailed, load-bearing):**

> "Don't add features, refactor code, or make 'improvements' beyond what was asked. A bug fix doesn't need surrounding code cleaned up. A simple feature doesn't need extra configurability. Don't add docstrings, comments, or type annotations to code you didn't change."
> "Don't create helpers, utilities, or abstractions for one-time operations. Don't design for hypothetical future requirements. Three similar lines of code is better than a premature abstraction."

**Backwards-compat prevention:**

> "Avoid backwards-compatibility hacks like renaming unused `_vars`, re-exporting types, adding `// removed` comments for removed code, etc. If something is unused, delete it completely."

**No timelines:**

> "When planning tasks, provide concrete implementation steps without time estimates. Never suggest timelines like 'this will take 2-3 weeks'."

### Tool Usage

- Explicit parallel execution policy: "If you intend to call multiple tools and there are no dependencies between them, make all independent tool calls in parallel."
- Task tool for exploration: "When exploring the codebase to gather context... it is CRITICAL that you use the Task tool with subagent_type=Explore"
- Specialized tools over bash: "Use dedicated tools: Read for reading files instead of cat/head/tail, Edit for editing instead of sed/awk"
- "NEVER use bash echo or other command-line tools to communicate thoughts"

### Safety/Guardrails

- Defensive security only (malware, DoS, supply chain attacks refused)
- Dual-use security tools require authorization context
- Never generate/guess URLs
- OWASP awareness: "Be careful not to introduce security vulnerabilities such as command injection, XSS, SQL injection"

### Unique Patterns

- **Colon prohibition before tool calls:** "Do not use a colon before tool calls. Text like 'Let me read the file:' followed by a read tool call should just be 'Let me read the file.' with a period." -- Prevents awkward formatting when tool calls are hidden from user.
- **Unlimited context claim:** "The conversation has unlimited context through automatic summarization."
- **Hook system awareness:** Treats hook feedback as coming from user.
- **TodoWrite as mandatory planning tool:** Two detailed examples showing step-by-step progress tracking.

---

## 2. Gemini CLI

**Source:** google-gemini/gemini-cli repo (packages/core/src/prompts/snippets.ts), open source. Also gist by ksprashu (Jul 2025 snapshot -- older version, pre-modular).

### Structure (composed from modular snippets)

1. **Preamble** -- "You are Gemini CLI, an interactive CLI agent specializing in software engineering tasks"
2. **Core Mandates** -- Security, context efficiency, engineering standards, expertise alignment
3. **Sub-Agents** -- XML-formatted available sub-agents
4. **Agent Skills** -- Skill activation system
5. **Hook Context** -- Read-only data handling
6. **Primary Workflows** -- Research -> Strategy -> Execution lifecycle, New Applications
7. **Operational Guidelines** -- Tone/style, security, tool usage
8. **Sandbox** -- macOS seatbelt / generic / outside
9. **YOLO Mode** -- Autonomous operation instructions
10. **Git Repository** -- Commit workflow
11. **User Memory** -- Hierarchical context (global > extension > project)

### Key Behavioral Instructions

**Directive vs. Inquiry distinction (unique, sophisticated):**

> "Distinguish between Directives (unambiguous requests for action or implementation) and Inquiries (requests for analysis, advice, or observations). Assume all requests are Inquiries unless they contain an explicit instruction to perform a task. For Inquiries, your scope is strictly limited to research and analysis; you MUST NOT modify files until a corresponding Directive is issued."

**Context efficiency mandate:**

> "Always scope and limit your searches to avoid context window exhaustion and ensure high-signal results. Use include to target relevant files and strictly limit results using total_max_matches and max_matches_per_file."

**Bug fix reproduction requirement:**

> "For bug fixes, you must empirically reproduce the failure with a new test case or reproduction script before applying the fix."

**Testing mandate:**

> "ALWAYS search for and update related tests after making a code change. You must add a new test case to the existing test file (if one exists) or create a new test file to verify your changes."

**Explain before acting (Gemini 3 specific):**

> "Never call tools in silence. You MUST provide a concise, one-sentence explanation of your intent or strategy immediately before executing tool calls."

### Tool Usage

- Parallel execution: "Execute multiple independent tool calls in parallel when feasible"
- Confirmation protocol: "If a tool call is declined or cancelled, respect the decision immediately. Do not re-attempt the action or 'negotiate' for the same tool call."
- Background processes: guidance on using & for long-running commands
- Interactive commands: avoid git rebase -i and similar

### Safety/Guardrails

- Credential protection: "Never log, print, or commit secrets, API keys, or sensitive credentials"
- Source control: "Do not stage or commit changes unless specifically requested"
- Sandbox awareness with user-facing guidance about seatbelt profiles

### Unique Patterns

- **Modular prompt composition:** Built from ~15 TypeScript functions (renderPreamble, renderCoreMandates, etc.) with conditional sections. Model-specific: Gemini 3 models get additional instructions (explain before acting).
- **Hierarchical context precedence:** Sub-directories > Workspace Root > Extensions > Global, with explicit conflict resolution rules.
- **Plan Mode as approval mode:** Separate workflow that restricts to read-only tools, writes plans to a plans directory.
- **YOLO mode:** Explicit autonomous operation with minimal interruption.
- **New Applications workflow:** Detailed guidance for building new apps from scratch, including tech stack preferences (React, Next.js, Flutter) and visual design requirements.
- **Per-model prompt variants:** Different prompt sets for Gemini 3 vs legacy models via snippets vs legacySnippets.
- **Validation as philosophy:** "Validation is the only path to finality. Never assume success or settle for unverified changes."

---

## 3. OpenCode

**Source:** anomalyco/opencode repo (packages/opencode/src/session/prompt/), open source TypeScript. Multiple prompt variants per model family.

### Structure (Anthropic variant -- primary)

1. **Identity** -- "You are OpenCode, the best coding agent on the planet"
2. **URL restriction** -- Same as Claude Code
3. **Help/feedback** -- ctrl+p actions, issue reporting
4. **Tone and style** -- No emoji, concise CLI, GFM markdown
5. **Professional objectivity** -- Anti-sycophancy
6. **Task Management** -- TodoWrite with examples
7. **Doing tasks** -- Workflow guidance
8. **Tool usage policy** -- Parallel, Task tool for exploration, specialized tools
9. **Code References** -- file_path:line_number

### Per-Model Variants

OpenCode maintains different prompts per model family:

| Model       | Prompt File      | Key Differences                                                                          |
| ----------- | ---------------- | ---------------------------------------------------------------------------------------- |
| Claude      | anthropic.txt    | TodoWrite, Task tool, professional objectivity                                           |
| GPT-5       | codex_header.txt | apply_patch editing, detailed editing constraints, git hygiene, frontend design guidance |
| Gemini      | gemini.txt       | Mirrors Gemini CLI's style (core mandates, workflows)                                    |
| GPT-4/o1/o3 | beast.txt        | Unknown (separate variant)                                                               |
| Qwen/other  | qwen.txt         | TodoWrite stripped out                                                                   |

### Key Behavioral Instructions (Codex/GPT-5 variant)

**Editing constraints:**

> "Default to ASCII when editing or creating files. Only introduce non-ASCII or other Unicode characters when there is a clear justification."

**Git workspace hygiene (detailed, practical):**

> "You may be in a dirty git worktree. NEVER revert existing changes you did not make unless explicitly requested... If the changes are in files you've touched recently, you should read carefully and understand how to work with the changes rather than reverting them."

**Frontend design guidance (unique):**

> "When doing frontend design tasks, avoid collapsing into bland, generic layouts. Aim for interfaces that feel intentional and deliberate."
> Specific guidance on typography, color (no purple bias, no dark mode bias), motion, background patterns.

**Question policy (practical):**

> "Questions: only ask when you are truly blocked after checking relevant context AND you cannot safely pick a reasonable default."
> Three specific blocking conditions: materially ambiguous, destructive/irreversible, need credential.

### Unique Patterns

- **Per-model prompt adaptation** -- The most sophisticated multi-model system. Different prompts genuinely optimize for each model family's strengths.
- **Heavily derived from Claude Code** -- The Anthropic variant is nearly identical to Claude Code's prompt, with branding swapped. Validates Claude Code as the template other agents copy.
- **Frontend design instructions** -- Only agent with explicit visual design guidance.
- **Output formatting as separate system** -- Detailed rules for final answer structure, file references, bullet formatting.
- **No permission questions:** "Never ask permission questions like 'Should I proceed?' or 'Do you want me to run tests?'; proceed with the most reasonable option and mention what you did."

---

## 4. Codex CLI

**Source:** Leaked prompt (gist by chigkim), open-source repo at openai/codex.

### Structure (in order)

1. **Identity** -- "You are a coding agent running in the Codex CLI, a terminal-based coding assistant"
2. **Capabilities** -- Explicit list: receive prompts, stream thinking, emit function calls
3. **Personality** -- Concise, direct, friendly
4. **AGENTS.md spec** -- Detailed scoping rules for AGENTS.md files
5. **Responsiveness** -- Preamble message guidance with examples
6. **Planning** -- update_plan tool usage with quality examples
7. **Task execution** -- Keep going until resolved, apply_patch format
8. **Sandbox and approvals** -- Filesystem/network sandboxing, approval modes
9. **Validating your work** -- Testing philosophy
10. **Tools** -- XML-formatted tool definitions inline

### Key Behavioral Instructions

**AGENTS.md scoping (most detailed spec):**

> "The scope of an AGENTS.md file is the entire directory tree rooted at the folder that contains it. For every file you touch in the final patch, you must obey instructions in any AGENTS.md file whose scope includes that file."
> "More-deeply-nested AGENTS.md files take precedence in the case of conflicting instructions."
> "Direct system/developer/user instructions take precedence over AGENTS.md instructions."

**Preamble messages (unique, with examples):**

> "Before making tool calls, send a brief preamble to the user explaining what you're about to do."
> Examples: "I've explored the repo; now checking the API route definitions." / "Config's looking tidy. Next up is patching helpers to keep things in sync."
> Personality in preambles: "Keep your tone light, friendly and curious"

**Plan quality examples (very effective):**
Shows both high-quality and low-quality plan examples, making the quality bar concrete:

- High: "Add CLI entry with file args -> Parse Markdown via CommonMark -> Apply semantic HTML template -> Handle code blocks, images, links -> Add error handling"
- Low: "Create CLI tool -> Add Markdown parser -> Convert to HTML"

**Sandbox system (most detailed):**
Four approval modes (untrusted, on-failure, on-request, never) with detailed behavioral guidance for each. The "never" mode is notable:

> "This is a non-interactive mode where you may NEVER ask the user for approval to run commands. Instead, you must always persist and work around constraints to solve the task for the user."

### Tool Usage

- Uses apply_patch exclusively for file editing (patch format inline in prompt)
- No explicit parallel tool call policy
- update_plan for progress tracking (not TodoWrite)
- Inline XML tool definitions in the prompt itself

### Safety/Guardrails

- Filesystem sandboxing: read-only, workspace-write, danger-full-access
- Network sandboxing: restricted, enabled
- Approval escalation system
- "NEVER add copyright or license headers unless specifically requested"
- "Do not git commit your changes or create new git branches unless explicitly requested"

### Unique Patterns

- **apply_patch format in prompt:** The actual patch format syntax is embedded directly in the system prompt, not delegated to tool descriptions.
- **Plan quality examples:** Only agent showing explicit good vs. bad examples.
- **Non-interactive mode guidance:** Detailed instructions for autonomous operation when user can't be asked.
- **Inline tool definitions:** XML tool schemas are part of the prompt, not a separate tools parameter.
- **Tool call format specification:** Custom XML format for function calls (not JSON).
- **Preamble personality guidance:** Encouraging light, curious tone in status updates.

---

## 5. Amp (Sourcegraph)

**Source:** Multiple versions. Aug 2025 (wunderwuzzi23/scratch), Oct 2025 (gist by gregce), Jan 2026 GPT-5 variant (x1xhlol repo).

### Structure (Aug 2025 version, in order)

1. **Identity** -- "You are Amp, a powerful AI coding agent built by Sourcegraph"
2. **Agency** -- Balance initiative with restraint
3. **Examples** -- Extensive tool use examples
4. **Task Management** -- todo_write/todo_read (mandatory)
5. **Conventions & Rules** -- Mimic style, verify libraries, security
6. **AGENT.md file** -- Context file handling
7. **Context** -- attachedFiles, user-state tags
8. **Communication** -- General, code comments, citations, conciseness
9. **Environment** -- Runtime details appended

### Structure (Jan 2026 GPT-5 variant, evolved)

1. **Role & Agency** -- End-to-end completion mandate
2. **Guardrails** -- Simple-first, reuse-first, no surprise edits, no new deps
3. **Fast Context Understanding** -- Parallel discovery, early stopping
4. **Parallel Execution Policy** -- What to parallelize vs serialize
5. **Tools and function calls** -- Rules, TODO tool, subagents (Task, Oracle, Codebase Search)
6. **AGENTS.md auto-context** -- Brief
7. **Quality Bar** -- Style, typing, tests, reuse
8. **Verification Gates** -- Typecheck -> Lint -> Tests -> Build
9. **Handling Ambiguity** -- Search before asking, present options
10. **Markdown Formatting Rules** -- Strict output formatting

### Key Behavioral Instructions

**End-to-end completion (Jan 2026, strong):**

> "Do the task end to end. Don't hand back half-baked work. FULLY resolve the user's request and objective. Keep working through the problem until you reach a complete solution -- don't stop at partial answers or 'here's how you could do it' responses."

**Guardrails section (Jan 2026, unique, effective):**

> - Simple-first: prefer the smallest, local fix over a cross-file "architecture change".
> - Reuse-first: search for existing patterns; mirror naming, error handling, I/O, typing, tests.
> - No surprise edits: if changes affect >3 files or multiple subsystems, show a short plan first.
> - No new deps without explicit user approval.

**Fast Context Understanding (unique optimization):**

> "Goal: Get enough context fast. Parallelize discovery and stop as soon as you can act."
> "Early stop (act if any): You can name exact files/symbols to change. You can repro a failing test/lint or have a high-confidence bug locus."
> "Trace only symbols you'll modify or whose contracts you rely on; avoid transitive expansion."

**Minimize reasoning (Jan 2026 GPT-5):**

> "MINIMIZE REASONING: Avoid verbose reasoning blocks throughout the entire session. Think efficiently and act quickly. Before any significant tool call, state a brief summary in 1-2 sentences maximum."

### Tool Usage

**Parallel execution policy (most detailed of any agent):**

> "Default to parallel for all independent work: reads, searches, diagnostics, writes and subagents."
> "Serialize only when there is a strict dependency."

Specific guidance on what to parallelize:

- Reads/searches/diagnostics: independent calls
- Codebase search agents: different concepts/paths in parallel
- Oracle: distinct concerns in parallel
- Task executors: parallel iff write targets are disjoint
- Independent writes: parallel iff disjoint

When to serialize:

- Plan -> Code: planning must finish before dependent edits
- Write conflicts: same file(s) or shared contract
- Chained transforms: step B requires artifacts from step A

**Three-tier subagent system:**

- **Oracle:** "Senior engineering advisor with o3 reasoning model for reviews, architecture, deep debugging, and planning"
- **Task:** "Fire-and-forget executor for heavy, multi-file implementations. Think of it as a productive junior engineer who can't ask follow-ups once started"
- **Codebase Search:** "Smart code explorer that locates logic based on conceptual descriptions across languages/layers"

Recommended workflow: Oracle (plan) -> Codebase Search (validate scope) -> Task (execute)

### Safety/Guardrails

- Redaction markers: "[REDACTED:amp-token]" handling -- must not overwrite secrets with markers
- No new dependencies without user approval
- Security best practices for secrets/keys

### Unique Patterns

- **Oracle tool:** Only agent with a dedicated "reasoning advisor" subagent. Users can consult a stronger reasoning model for architectural decisions.
- **Mermaid diagrams:** Agent proactively creates diagrams for architecture explanations.
- **No-flattery rule:** "You never start your response by saying a question or idea or observation was good, great, fascinating, profound, excellent, perfect, or any other positive adjective."
- **File URL linking:** All file references must use file:// URLs with line numbers. "Prefer fluent linking style."
- **Evolving prompt across versions:** Aug 2025 to Jan 2026 shows significant prompt evolution -- the GPT-5 variant is more opinionated about parallel execution, adds guardrails section, adds reasoning minimization.
- **Conciseness as hard limit:** "You MUST answer concisely with fewer than 4 lines (excluding tool use or code generation)"
- **Redaction awareness:** Explicit handling of security-redacted content in files.

---

## Cross-Cutting Patterns

### Universal Patterns (present in all 5)

1. **Concise CLI tone** -- All agents optimize for terminal display, minimize output
2. **No emoji** -- Universal prohibition (unless explicitly requested)
3. **GFM markdown** -- Standard output format
4. **Read before edit** -- Never propose changes to code you haven't read
5. **Security awareness** -- Don't expose secrets, keys, credentials
6. **Specialized tools over bash** -- Prefer Read/Edit/Write tools over cat/sed/echo
7. **Parallel tool execution** -- All support it, most explicitly instruct it
8. **AGENTS.md/CLAUDE.md/GEMINI.md** -- All support project-specific instruction files

### Convergent Patterns (present in 4 of 5)

1. **TodoWrite / plan tracking** -- All except Codex (which uses update_plan)
2. **Anti-sycophancy** -- Claude Code, OpenCode, Amp explicitly; Gemini implicitly
3. **Keep going until resolved** -- Explicit in Claude Code, Codex, Amp, OpenCode
4. **Task/subagent delegation** -- Claude Code, Gemini, OpenCode, Amp (not Codex)
5. **Git commit restraint** -- Never commit unless asked (all agents)
6. **No comments unless asked** -- Present in most agents

### Divergent Approaches

| Concern          | Claude Code               | Gemini CLI              | OpenCode          | Codex CLI           | Amp                   |
| ---------------- | ------------------------- | ----------------------- | ----------------- | ------------------- | --------------------- |
| File editing     | Edit tool                 | edit tool               | Edit/Write        | apply_patch         | edit_file             |
| Planning         | TodoWrite                 | write_todos             | TodoWrite         | update_plan         | todo_write            |
| Autonomy control | Hooks + ask tool          | Directive vs Inquiry    | Question policy   | Sandbox + approvals | Oracle consultation   |
| Code search      | Task(Explore)             | codebase_investigator   | Task tool         | shell (rg)          | codebase_search_agent |
| Response limit   | "short and concise"       | <3 lines                | varies by variant | not explicit        | <4 lines              |
| Subagent arch    | Explore/Plan/Bash/general | configurable sub-agents | Task/Explore      | none                | Task/Oracle/Search    |

---

## Effective Patterns Worth Adopting

### High-Impact, Low-Token

1. **Anti-over-engineering (Claude Code):** "Three similar lines of code is better than a premature abstraction." Extremely effective per token.

2. **Guardrails section (Amp Jan 2026):** Simple-first, reuse-first, no surprise edits, no new deps. Four rules that prevent the most common agentic failures.

3. **Fast Context Understanding (Amp):** Parallelize discovery, early stop when you can act. Reduces unnecessary exploration.

4. **No-flattery rule (Amp):** "You never start your response by saying a question or idea was good, great, fascinating..." More specific than Claude Code's anti-sycophancy.

5. **Colon prohibition (Claude Code):** Tiny instruction, prevents real UX issue.

### Medium-Impact, Higher Token Cost

6. **Plan quality examples (Codex):** Showing good vs bad plans makes the quality bar concrete.

7. **Parallel execution policy (Amp Jan 2026):** Detailed what-to-parallelize vs when-to-serialize guidance with examples.

8. **Directive vs. Inquiry distinction (Gemini):** Prevents premature action on questions. But adds complexity.

9. **Question policy (OpenCode GPT-5):** Three specific conditions for when to ask. Reduces unnecessary blocking.

10. **Preamble message examples (Codex):** Light, friendly status updates. Good UX.

### Patterns to Avoid

1. **Excessive examples** -- Amp Aug 2025 has ~15 examples. Token-expensive, most models don't need this many.
2. **Frontend design guidance** -- OpenCode GPT-5 variant includes typography/color guidance. Too domain-specific for a general agent.
3. **Inline tool definitions** -- Codex includes XML tool schemas in the prompt. Better as separate tools parameter.
4. **New Application workflows** -- Gemini/OpenCode have detailed "build new app" sections with tech stack preferences. Over-opinionated.

---

## Architecture Insights for ion

### What the survey shows

The most effective prompts share these properties:

- **Identity + role in 1-2 sentences** -- All agents do this
- **Failure mode corrections over ideal behavior descriptions** -- Anti-sycophancy, anti-over-engineering, no-flattery: all exist because models actually do these things
- **Tool routing guidance** -- When to use Task vs direct search, when to parallelize
- **Hard limits on output** -- "<3 lines", "<4 lines" work better than "be concise"
- **Examples for ambiguous instructions** -- Plan quality, preamble messages, tool use patterns

The least effective prompt patterns:

- **Describing behavior models already exhibit** -- "Be helpful" is wasted tokens
- **Domain-specific guidance** -- Frontend design rules, tech stack preferences
- **Excessive examples** -- Diminishing returns after 3-4 examples per concept

### Recommended adoptions for ion's prompt

| Pattern                                                   | Source          | Estimated Tokens | Priority |
| --------------------------------------------------------- | --------------- | ---------------- | -------- |
| Anti-over-engineering rules                               | Claude Code     | ~80              | HIGH     |
| Guardrails (simple-first, reuse-first, no surprise edits) | Amp             | ~50              | HIGH     |
| Parallel execution guidance                               | Amp/Claude Code | ~60              | HIGH     |
| No-flattery specific wording                              | Amp             | ~30              | MEDIUM   |
| Fast context understanding                                | Amp             | ~40              | MEDIUM   |
| Plan quality examples (good vs bad)                       | Codex           | ~120             | MEDIUM   |
| Question policy (when to ask)                             | OpenCode        | ~60              | MEDIUM   |
| Colon before tool calls prohibition                       | Claude Code     | ~30              | LOW      |

---

## Source Quality Notes

| Source                        | Quality                                                    | Freshness                  |
| ----------------------------- | ---------------------------------------------------------- | -------------------------- |
| google-gemini/gemini-cli repo | Official, open source                                      | Current (Feb 2026 commits) |
| anomalyco/opencode repo       | Official, open source (archived -> Crush)                  | Current dev branch         |
| chigkim gist (Claude Code)    | Leaked, verified against known behavior                    | Jan 2026                   |
| chigkim gist (Codex CLI)      | Leaked, matches openai/codex repo patterns                 | ~2025                      |
| wunderwuzzi23/scratch (Amp)   | Security researcher, verified via prompt injection testing | Aug 2025                   |
| x1xhlol repo (Amp GPT-5)      | Community collection, plausible but unverified             | Jan 2026                   |
| gregce gist (Amp)             | Community, matches other sources                           | Oct 2025                   |
