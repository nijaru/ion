# Feature Gap Analysis: Ion vs Claude Code vs Pi-mono vs Codex

**Research Date**: 2026-02-10
**Purpose**: Identify features that matter for task completion, prioritize gaps, find complexity to remove
**Sources**: Terminal-Bench 2.0, SWE-bench Verified, ACTI survey (271 devs, Jan 2026), Nuanced LSP eval (720 runs), Anthropic 2026 Trends Report, practitioner reports

---

## 1. Feature Matrix

### Core Features

| Feature                      | Ion          | Claude Code | Pi-mono   | Codex CLI | OpenCode  | Notes                                  |
| ---------------------------- | ------------ | ----------- | --------- | --------- | --------- | -------------------------------------- |
| Agent loop (multi-turn)      | Yes          | Yes         | Yes       | Yes       | Yes       | Table stakes                           |
| File tools (read/write/edit) | Yes          | Yes         | Yes       | Yes       | Yes       | Table stakes                           |
| Shell tool (bash)            | Yes          | Yes         | Yes       | Yes       | Yes       | Table stakes                           |
| Search tools (glob/grep)     | Yes          | Yes         | No (bash) | No (bash) | Yes       | Ion/CC have dedicated; others use bash |
| Project context (AGENTS.md)  | Yes          | Yes         | No        | Yes       | Yes       | Standard across tier-1                 |
| Session persistence          | Yes (SQL)    | Yes         | Yes       | Yes       | Yes       | Table stakes                           |
| Context compaction           | Yes (3-tier) | Yes         | No\*      | Yes       | Yes       | \*Pi relies on large context windows   |
| Multi-provider support       | Yes (7)      | No (1)      | Yes (15+) | No (1)    | Yes (75+) | Ion differentiator vs CC/Codex         |
| MCP client                   | Yes          | Yes (dual)  | No        | Yes       | Yes       | Ion has lazy loading                   |
| Streaming + tools            | Yes          | Yes         | Yes       | Yes       | Yes       | Table stakes for UX                    |
| Markdown rendering           | Yes          | Yes         | Yes       | Yes       | Yes       | Table stakes                           |
| Cost tracking                | Yes          | Yes         | No        | Yes       | No        | Useful but not critical                |
| Image attachment             | Yes          | Yes         | No        | Yes       | No        | Increasingly expected                  |

### Advanced Features

| Feature                 | Ion            | Claude Code   | Pi-mono   | Codex CLI   | Notes                                      |
| ----------------------- | -------------- | ------------- | --------- | ----------- | ------------------------------------------ |
| Skills/SKILL.md         | Yes (590 LOC)  | Yes           | No        | Yes         | Standard emerging                          |
| Sub-agents              | Designer only  | Yes (Task)    | No (bash) | Yes (6 max) | CC has most mature implementation          |
| Hooks system            | Framework      | Yes (shell)   | No        | No          | Ion: types defined, not wired to config    |
| Sandboxing (OS-level)   | No             | Yes (84%)     | No        | Yes         | CC: Seatbelt+net; Codex: Seatbelt+Landlock |
| Checkpoints/undo        | No             | Yes (/rewind) | No        | No          | CC only; Conductor built alternative       |
| Mid-turn steering       | Partial\*      | Yes           | No        | Yes         | \*Ion has message_queue but limited UX     |
| LSP integration         | No             | Yes           | No        | No          | OpenCode + Crush have it too               |
| Plan/designer mode      | Yes (auto)     | Yes           | No        | Yes (/plan) | Controversial -- see analysis              |
| Thinking mode toggle    | Yes            | Yes           | No        | No          | Ion: Ctrl+T cycles levels                  |
| Session resume          | Partial        | Yes           | Yes       | Yes         | Ion has SQL persistence, needs /resume     |
| Git integration         | Via bash       | Via bash      | Via bash  | Deep        | Codex has git safety, auto-commits         |
| Web search/fetch        | Yes (built-in) | Via MCP       | Via bash  | No          | Ion has native web tools                   |
| CLI/headless mode       | Partial        | Yes           | Yes       | Yes         | Ion has `ion run` designed, not complete   |
| Parallel tool execution | No             | Yes           | No        | Yes         | Codex: FuturesOrdered                      |
| Shareable sessions      | No             | No            | No        | No          | OpenCode has this                          |

### Permission & Security

| Feature             | Ion        | Claude Code | Pi-mono | Codex CLI    | Notes                                  |
| ------------------- | ---------- | ----------- | ------- | ------------ | -------------------------------------- |
| Read/Write modes    | Yes        | Yes         | No      | Yes (3-tier) | Ion: clean 2-mode design               |
| OS sandbox          | Designed\* | Yes         | No      | Yes          | \*Ion: Seatbelt+Landlock in design doc |
| Permission prompts  | Removed    | Sandboxed   | None    | Cached       | Ion's approach matches Pi's YOLO       |
| Deny rules (config) | Designed   | Yes         | No      | No           | Ion: glob patterns designed            |
| Git safety guards   | No         | Yes         | No      | Yes          | Block destructive git commands         |

---

## 2. What Matters for Task Completion

### Benchmark Evidence

**Terminal-Bench 2.0** (101 entries, Feb 2026):

| Rank | Agent        | Model           | Score | Agent Complexity |
| ---- | ------------ | --------------- | ----- | ---------------- |
| 1    | Simple Codex | GPT-5.3-Codex   | 75.1% | Minimal scaffold |
| 2    | CodeBrain-1  | GPT-5.3-Codex   | 70.3% | Custom           |
| 3    | Droid        | Claude Opus 4.6 | 69.9% | Full agent       |
| 7    | Terminus 2   | GPT-5.3-Codex   | 64.7% | tmux only        |

Key finding: **"Simple Codex" (minimal scaffold) beats "Droid" (full agent)**. Terminus 2 (just a tmux session, no tools, no file operations) "holds its own against agents with far more sophisticated tooling." This validates Pi-mono's thesis: **model quality >> feature count**.

**SWE-bench Verified** (Feb 2026):

- Claude Opus 4.6: 80.8%
- GPT-5.3-Codex: ~78% (across various scaffolds)
- Prediction: "~95% and then move on" (Andrew Zakonov) -- benchmark ceiling approaching

**Nuanced LSP Evaluation** (720 runs, 2 models, 10 SWE-bench tasks):

- Result: **Inconclusive/negative**. "External code intelligence is hard to turn into reliable wins because we do not control how models use it."
- > 50% reductions in some runs, ~50% improvements in others, under identical conditions
- Agents sometimes used LSP data effectively, sometimes "ignored or misapplied the same signals"

### Practitioner Evidence

**ACTI Survey** (271 developers, January 2026):

| Tool           | Adoption | Trend   |
| -------------- | -------- | ------- |
| Claude Code    | 69%      | +34 pts |
| ChatGPT/Chat   | 25.5%    | -5 pts  |
| Cursor         | 25.1%    | +5 pts  |
| GitHub Copilot | 21.8%    | -13 pts |
| Codex CLI      | 13.7%    | +6 pts  |

**What developers actually do with agents:**

1. Writing new code: 69.7%
2. Debugging: 45.4%
3. Refactoring: 29.5%
4. Understanding code: 17.3%
5. Writing tests: 16.6%

**Productivity correlation**: Heavy users (76%+ AI time) report 2.9x higher productivity gains than light users. Usage intensity > tool features.

**Addy Osmani (Anthropic)**: "Start with 2-3 essentials (CLAUDE.md, LSP, 1-2 MCPs), activate additional tools on-demand. The context window is the scarce resource."

### Feature-Performance Correlation

| Feature                  | Evidence of Impact                               | Confidence  |
| ------------------------ | ------------------------------------------------ | ----------- |
| Model quality            | Strong positive (TB 2.0: model >> scaffold)      | High        |
| Context efficiency       | Strong positive (Pi <1k tokens, competitive)     | High        |
| Basic tools (R/W/E/Bash) | Necessary and sufficient (Terminus 2)            | High        |
| AGENTS.md                | Positive (practitioner consensus)                | Medium-High |
| Sandboxing               | 84% fewer interruptions (CC data)                | High        |
| LSP                      | Inconclusive (Nuanced eval: noise > signal)      | Low         |
| Sub-agents               | Mixed (useful for isolation, black box risk)     | Medium      |
| Skills                   | Positive for progressive disclosure (~50 tokens) | Medium      |
| Checkpoints              | Positive for UX, no benchmark impact data        | Low-Medium  |
| Plan mode                | Controversial (Amp removed it, "just a prompt")  | Low         |
| MCP tools                | Negative when excessive (40%+ context)           | Medium      |
| Compaction               | Essential for long sessions                      | High        |
| Mid-turn steering        | UX improvement, no task completion data          | Low         |

---

## 3. Ion's Differentiators

### Unique Strengths

| Differentiator            | Detail                                                                                              | Competitive Position                              |
| ------------------------- | --------------------------------------------------------------------------------------------------- | ------------------------------------------------- |
| **Multi-provider**        | 7 providers (Anthropic, Google, Groq, Kimi, Ollama, OpenAI, OpenRouter) with native streaming+tools | Unique vs CC/Codex; OpenCode has more (75+)       |
| **Rust native**           | Single binary, fast startup, low memory                                                             | Shared with Codex only                            |
| **3-tier compaction**     | Prune -> summarize -> LLM (graduated approach)                                                      | More sophisticated than competitors               |
| **Context efficiency**    | Native provider protocol handling, no framework overhead                                            | Comparable to Pi-mono                             |
| **Custom TUI**            | Direct crossterm, native scrollback, no framework                                                   | Validated approach (Codex, CC all converged here) |
| **Web tools built-in**    | web_search, web_fetch as native tools                                                               | CC uses MCP for this                              |
| **Permission simplicity** | Read/Write modes, no approval prompts                                                               | Clean, matches practitioner preference            |
| **Provider-side caching** | Stable system prompt prefix for Anthropic/Google cache hits                                         | Cost optimization unique to ion                   |

### Positioning

Ion sits between Pi-mono (extreme minimalism) and Claude Code (feature maximalism). Its multi-provider support and Rust performance are real differentiators. The question is whether to move toward Pi's simplicity or CC's features.

**Recommendation**: Move toward Pi. Benchmark data shows model quality and context efficiency dominate feature richness. Ion should be the **fast, efficient, multi-provider** agent -- not a CC clone.

---

## 4. Missing Features Prioritized by Impact

### P1: Critical for Competitive Parity

| Feature               | Effort | Impact | Rationale                                                                                                                                                  |
| --------------------- | ------ | ------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **OS-level sandbox**  | Medium | High   | 84% reduction in interruptions (CC data). Design doc exists (`ai/design/permissions-v2.md`). macOS Seatbelt + Linux Landlock. Enables confident YOLO mode. |
| **Session resume**    | Low    | High   | `/resume` or `/continue` to pick up last session. SQL persistence exists; need UX integration. Every competitor has this.                                  |
| **CLI headless mode** | Medium | High   | `ion run "prompt"` designed in DECISIONS.md but not shipped. Required for CI/CD, scripting, automation.                                                    |

### P2: Meaningful Improvement

| Feature                     | Effort | Impact      | Rationale                                                                                                                                                        |
| --------------------------- | ------ | ----------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Parallel tool execution** | Medium | Medium-High | Codex uses FuturesOrdered. Multi-file reads in parallel. Direct speedup for every session.                                                                       |
| **Git safety guards**       | Low    | Medium      | Block `git push --force`, `git reset --hard` unless explicit. Small code, prevents catastrophic errors.                                                          |
| **Hooks wiring**            | Low    | Medium      | Framework exists (426 LOC, tested). Need config parsing (`[[hooks]]` in TOML) and integration into ToolOrchestrator. Enables `cargo fmt` on write, linting, etc. |
| **Mid-turn steering UX**    | Low    | Medium      | `message_queue` infrastructure exists. Need clear UX: show "Type to redirect..." when agent is running, visual indicator of queued message.                      |

### P3: Nice to Have

| Feature                  | Effort | Impact     | Rationale                                                                                                                                                   |
| ------------------------ | ------ | ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Checkpoints**          | High   | Medium     | `/rewind` to restore code + conversation. Useful for experimentation. Complex to implement correctly (Conductor's blog shows CC's checkpoints are "leaky"). |
| **Sub-agents (general)** | High   | Medium     | Beyond designer: Task tool for context isolation. High implementation cost. Pi's approach (spawn via bash) is simpler and provides full observability.      |
| **Extension packaging**  | Medium | Low-Medium | `ion plugin add <git-url>` for skills + MCP config + hooks. Nice ecosystem play but premature without users.                                                |

### P4: Low Priority / Skip

| Feature                | Effort    | Impact    | Rationale                                                                                                                                                                                      |
| ---------------------- | --------- | --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **LSP integration**    | Very High | Uncertain | Nuanced eval: 720 runs, inconclusive results. "External code intelligence is hard to turn into reliable wins." Models are improving at code understanding natively. Wait for clearer evidence. |
| **Persistent memory**  | Very High | Unknown   | No agent has shipped this successfully. CC's memory tool is "still beta, mixed reviews." OmenDB requires nightly Rust. Deferred correctly.                                                     |
| **Shareable sessions** | Medium    | Low       | OpenCode has this. Nice for collaboration but not a coding performance feature.                                                                                                                |
| **MCP server mode**    | Medium    | Low       | CC has this (dual MCP role). Minimal demand for agents to be MCP servers.                                                                                                                      |

---

## 5. Features to Remove or Simplify

### Designer/Plan Mode: Simplify or Remove

**Current state**: `Designer` sub-agent (134 LOC in `designer.rs`) auto-triggers for long messages (>100 chars). Generates JSON task graph with `PlannedTask` and `TaskStatus`. Injected into context via `ContextManager`.

**Evidence against**:

- Amp explicitly removed plan mode: "Plan mode is just a prompt. No structural difference from asking the model to write a file."
- Pi-mono: "Write plans to files for cross-session persistence."
- Armin Ronacher (Flask creator): Detailed analysis of why plan mode is counterproductive
- Practitioner consensus: Built-in plan modes spawn sub-agents with zero visibility
- Auto-trigger on message length is fragile (a long paste is not a complex request)

**Recommendation**: **Simplify to prompt-only**. Remove the `Designer` struct, `Plan`, `PlannedTask`, `TaskStatus` types. If users want planning, they can ask: "Plan how to implement this. Do NOT write code." The 134 LOC adds complexity, uses a non-streaming API call (cost + latency), and the auto-trigger heuristic (>100 chars) creates surprising behavior.

**Alternative**: Keep as opt-in `/plan` command without auto-trigger. Remove the auto-trigger entirely.

### Skills System: Keep but Audit Usage

**Current state**: 590 LOC in `src/skill/mod.rs`. Supports YAML frontmatter and legacy XML. Progressive loading (summaries at startup, full load on demand). Registry pattern.

**Assessment**: Well-implemented, follows agentskills.io spec, compatible with Claude Code and Crush ecosystems. Progressive disclosure is the right pattern (~50 tokens at startup vs full prompt on demand).

**Issue**: No built-in skills ship with ion. The infrastructure exists but there is no content. Without default skills, the system is dead weight.

**Recommendation**: **Keep, but ship 2-3 default skills** (e.g., `code-review`, `refactor`). Or document how users create skills. If no skills exist after 3 months, consider removing.

### Hook System: Wire or Remove

**Current state**: 426 LOC in `src/hook/mod.rs`. Types defined (`HookPoint`, `HookContext`, `HookResult`), `CommandHook` implementation exists, tests pass. But: only `PreToolUse` and `PostToolUse` points (design doc specifies more: `SessionStart`, `Stop`, `PreCompact`). Not wired to config parsing or `ToolOrchestrator`.

**Recommendation**: **Wire it up (P2)**. The framework is 80% complete. Add TOML config parsing and integrate with `ToolOrchestrator.call_tool()`. This enables `cargo fmt` after writes, linting hooks, safety checks -- high value for low effort. If not wired within 2 sprints, remove to reduce dead code.

### Web Tools: Keep

**Current state**: `web_search` and `web_fetch` as built-in tools.

**Assessment**: Native web tools avoid MCP overhead. CC requires an MCP server for web access. This is a genuine advantage.

**Recommendation**: Keep. Consider if they are context-efficient (progressive: search returns snippets, fetch loads full page only when requested).

---

## 6. The Convergent Minimum

Based on benchmark data, practitioner reports, and the convergence pattern across all successful agents:

### Must Have (Minimum Viable Competitive Agent)

```
1. Agent loop (multi-turn, while model produces tool calls)
2. File tools: read, write, edit
3. Shell tool: bash (with kill_on_drop, timeout)
4. Search tools: glob, grep (or agents can use bash for this)
5. Project context: AGENTS.md loading
6. Session persistence (resume capability)
7. Context compaction (auto-summarize near limit)
8. Streaming responses with tool calling
9. Markdown rendering in terminal
10. Sandbox (OS-level filesystem + network isolation)
```

**Token budget**: <2,000 tokens for system prompt + tool definitions. Every token competes for reasoning space.

### Should Have (Meaningful Differentiation)

```
11. Multi-provider support (the user picks their model)
12. Skills system (progressive disclosure, <50 tokens at startup)
13. Hooks (pre/post tool execution, config-driven)
14. Cost tracking (token usage, model pricing)
15. CLI/headless mode (scripting, CI/CD)
16. Parallel tool execution
17. Git safety (block destructive commands)
18. Thinking mode support (extended reasoning)
```

### Could Have (Nice but Unproven)

```
19. Sub-agents (context isolation, but adds complexity)
20. Checkpoints/undo (UX improvement, hard to implement correctly)
21. LSP integration (data inconclusive)
22. Plan mode (just a prompt, don't build infrastructure)
23. Mid-turn steering (UX improvement, unclear demand)
24. Image support (screenshots, diagrams)
25. Web tools (built-in search/fetch)
```

### Should Not Have (Complexity Without Value)

```
- Built-in memory system (no agent has made this work)
- MCP server mode (minimal demand)
- Plugin marketplace (premature)
- WASM plugin host (no ecosystem)
- Full-screen TUI (terminal scrollback is better)
- Complex permission systems (sandbox > prompts)
- Auto-triggering plan mode (just a prompt)
```

---

## 7. Prioritized Action List

### Sprint-Ready (Can Start Now)

| #   | Action                           | Effort   | Files                                                     | Impact                                     |
| --- | -------------------------------- | -------- | --------------------------------------------------------- | ------------------------------------------ |
| 1   | **Ship OS sandbox**              | 2-3 days | `src/tool/builtin/bash.rs`                                | P1: 84% fewer interruptions                |
| 2   | **Wire hooks to config**         | 1 day    | `src/hook/mod.rs`, `src/config/mod.rs`, `src/tool/mod.rs` | P2: Enables cargo fmt, linting             |
| 3   | **Add `/resume` command**        | 1 day    | `src/tui/events.rs`, `src/session/`                       | P1: Session continuity                     |
| 4   | **Add git safety guards**        | 0.5 day  | `src/tool/builtin/guard.rs`                               | P2: Prevent `--force`, `reset --hard`      |
| 5   | **Remove designer auto-trigger** | 0.5 day  | `src/agent/mod.rs`                                        | Simplification: remove surprising behavior |
| 6   | **Polish mid-turn steering UX**  | 1 day    | `src/tui/events.rs`, `src/tui/render/`                    | P2: message_queue exists, needs UX         |

### Next Sprint

| #   | Action                           | Effort   | Impact                           |
| --- | -------------------------------- | -------- | -------------------------------- |
| 7   | **Ship `ion run` headless mode** | 3-5 days | P1: CI/CD, scripting             |
| 8   | **Parallel tool execution**      | 2-3 days | P2: Direct speedup               |
| 9   | **Ship 2-3 default skills**      | 1 day    | Activate existing infrastructure |

### Backlog (Needs More Evidence)

| #   | Action             | Condition                                                           |
| --- | ------------------ | ------------------------------------------------------------------- |
| 10  | LSP integration    | Wait for stronger eval data; models may solve this natively         |
| 11  | General sub-agents | Wait for user demand; Pi's bash approach may suffice                |
| 12  | Checkpoints        | Wait for clean implementation pattern (Conductor's approach > CC's) |
| 13  | Persistent memory  | Wait for stable Rust OmenDB or alternative                          |

---

## Summary

**Ion's position is strong.** Multi-provider support, Rust performance, context efficiency, and clean permissions design are genuine differentiators. The biggest gaps are operational (sandbox, session resume, headless mode) not architectural.

**The main risk is feature bloat, not feature gaps.** Terminal-Bench data shows minimal scaffolds outperform complex agents. Pi-mono with 4 tools is competitive. The path forward is: ship the sandbox, wire the hooks, polish session resume, and resist adding features that don't clearly improve task completion.

**Model quality dominates.** The same agent scaffold scores 75.1% with GPT-5.3-Codex and 3.1% with a weak model. No amount of features compensates for model quality. Ion's multi-provider support -- letting users pick the best available model -- is therefore its most important architectural decision.

---

## Change Log

- 2026-02-10: Initial analysis from Terminal-Bench 2.0, SWE-bench, ACTI survey, Nuanced LSP eval, practitioner reports
