# Coding TUI/CLI Agents: State of the Art (February 2026)

**Research Date**: 2026-02-09
**Purpose**: Comprehensive survey of coding agent landscape, emerging patterns, and technical details
**Prior research**: agent-survey.md (Jan 2026), tui-hybrid-chat-libraries-2026.md

---

## 1. Amp (Sourcegraph -> Amp Inc.)

### Corporate Status

Sourcegraph and Amp split into independent companies in December 2025. Quinn Slack and Beyang Liu founded Amp Inc. with the Amp team. Cody Free and Cody Pro were discontinued in July 2025. Amp is now the sole focus.

### Philosophy: Aggressive Subtraction

Amp's core principle is ruthless feature removal. They operate on four stated principles:

1. Unconstrained token usage
2. Always uses the best models
3. Gives you raw model power
4. Built to evolve with new models

They maintain a "Frequently Ignored Feedback" section -- explicitly documenting user requests they refuse to implement. Their stance: "the best product is built by iterating fast at the model<->product frontier."

### Features Removed (January 2026)

| Feature                         | Date Removed | Rationale                                                                                   |
| ------------------------------- | ------------ | ------------------------------------------------------------------------------------------- |
| **Amp Tab** (inline completion) | Jan 15, 2026 | "The era of tab completion is coming to an end. We're entering the post-agentic age."       |
| **Fork command**                | Jan 13, 2026 | Replaced by handoff + thread mentions as first-class features                               |
| **TODO list**                   | Jan 12, 2026 | "With Opus 4.5, we found it's no longer needed" -- models track work without explicit lists |
| **Custom commands**             | Jan 29, 2026 | Consolidated into skills, eliminating redundancy                                            |
| **Isolated mode / BYOK**        | May 2025     | Prioritizes rapid model-product co-evolution over user-controlled models                    |

### Plan Mode Removal

Amp removed plan mode because:

- **Plan mode is just a prompt.** A plan in Claude Code is effectively a markdown file. No structural difference from asking the model to write a file.
- **Lack of observability.** Built-in plan modes spawn sub-agents with zero visibility. Users cannot see what sources the agent examined.
- **Friction with auto-approve.** Plan mode didn't inherit tool permissions, forcing approval prompts in YOLO mode.
- **Better alternative: just ask.** "Only plan how to implement this. Do NOT write any code." makes plan mode unnecessary as a feature.

### What Replaced Plan Mode

Amp introduced three modes instead:

- **Smart**: Unconstrained state-of-the-art model use (default)
- **Rush**: Faster, cheaper, for well-defined small tasks
- **Deep**: Extended thinking for complex problems -- "thinks for longer, plans more, needs you less"

### Current Feature Set

- Oracle (reasoning/review agent)
- Sub-agents with agents panel for managing threads
- Skills (replaced custom commands)
- Code review agent (composable and extensible)
- Shareable walkthroughs (interactive annotated diagrams)
- Handoff (move to new thread preserving context)
- Painter tool (image generation/editing)
- CLI + web UI for thread sharing

### Sources

- [Amp Chronicle](https://ampcode.com/chronicle)
- [Amp Owner's Manual](https://ampcode.com/manual)
- [Sourcegraph/Amp Split Announcement](https://sourcegraph.com/blog/why-sourcegraph-and-amp-are-becoming-independent-companies)
- [Armin Ronacher on Plan Mode](https://lucumr.pocoo.org/2025/12/17/what-is-plan-mode/)

---

## 2. Pi-Mono / OpenCode

### Pi-Mono (badlogic/Mario Zechner)

**Repository**: https://github.com/badlogic/pi-mono
**Stars**: ~1.9k (niche but influential)
**Philosophy**: "If I don't need it, won't build it"

#### Why It's Effective Despite Being Minimal

1. **System prompt < 1,000 tokens** vs. thousands in competitors. "Frontier models have been RL-trained up the wazoo, so they inherently understand what a coding agent is."
2. **Only 4 tools**: read, write, edit, bash. Nothing else.
3. **No MCP**: Popular MCP servers (e.g., Playwright MCP: 21 tools, 13.7k tokens) dump tool descriptions into context every session. Pi uses CLI tools with READMEs -- agents read docs on-demand, paying token cost only when needed.
4. **No sub-agents**: "When Claude Code spawns a sub-agent, you have zero visibility." Instead, spawn pi itself via bash/tmux for full observability.
5. **No plan mode**: Write plans to files for cross-session persistence.
6. **No permission checks**: Full YOLO mode by default.

#### Architecture

```
pi-mono/
  packages/
    pi-ai/           # Unified LLM API (15+ providers)
    pi-agent-core/   # Agent loop, state management
    pi-tui/          # Differential TUI (retained-mode, scrollback)
    pi-coding-agent/ # CLI, session management, tools
    pi-mom/          # Slack bot
    pi-web-ui/       # Web components
    pi-pods/         # vLLM deployment
```

SDK philosophy: "Omit to discover, provide to override." Omit an option and pi discovers/loads from standard locations; provide an option and your value overrides.

#### Benchmark Validation

Terminal-Bench 2.0: Pi with Claude Opus 4.5 competes effectively against specialized harnesses. **Terminus 2** (just a tmux session, no tools, no file operations) "holds its own against agents with far more sophisticated tooling" -- validating the minimal approach.

### OpenCode (Anomaly)

**Repository**: https://github.com/anomalyco/opencode
**Stars**: 99.4k (massive growth from 75k in Jan 2026)
**Monthly active users**: 650,000+
**Contributors**: 550+

#### What Makes It Effective

| Feature                | Detail                                                              |
| ---------------------- | ------------------------------------------------------------------- |
| **OpenTUI**            | Custom in-house TUI framework (8.3k stars separately)               |
| **LSP integration**    | Auto-detects and configures best LSPs per language                  |
| **75+ providers**      | Via models.dev, including local models                              |
| **Multi-session**      | Parallel agents on same project without conflict                    |
| **Client/server arch** | TUI is just one frontend; desktop app, IDE extension also available |
| **Privacy-first**      | No code/context stored externally                                   |
| **Share links**        | Instant shareable session URLs                                      |

#### Built-in Agents

| Agent       | Purpose              | Restrictions        |
| ----------- | -------------------- | ------------------- |
| **build**   | Default, full access | None                |
| **plan**    | Read-only analysis   | Denies edits        |
| **general** | Complex searches     | Invoke via @general |

#### Architecture

Built with TypeScript, npm workspaces. The client/server split means the backend can run locally while driven remotely (mobile app). GitHub Actions integration for automated workflows.

### Sources

- [Pi-Mono Blog Post](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/)
- [Pi-Mono GitHub](https://github.com/badlogic/pi-mono)
- [OpenCode GitHub](https://github.com/anomalyco/opencode)
- [OpenCode InfoQ](https://www.infoq.com/news/2026/02/opencode-coding-agent/)
- [OpenCode Grokipedia](https://grokipedia.com/page/OpenCode)

---

## 3. Claude Code (Anthropic)

### What Actually Gets Used

Based on practitioner reports (January 2026):

**Essential (high-value features)**:
| Feature | Why |
| --- | --- |
| **LSP integration** | "Single biggest productivity gain" -- semantic code understanding vs. text files |
| **CLAUDE.md / AGENTS.md** | Project context that persists across sessions |
| **Skills (SKILL.md)** | Progressive disclosure: ~50 tokens at startup, full load on demand |
| **Sub-agents (Task tool)** | Context isolation for parallel work |
| **Sandboxing** | 84% reduction in permission prompts |
| **Hooks** | Pre/post tool execution hooks for custom workflows |
| **Checkpoints** | State save/restore |
| **Context compaction** | Auto-summarization near context limit |

**Frequently cited as bloat / problematic**:
| Feature | Issue |
| --- | --- |
| **Excessive MCP tools** | Can consume 40%+ of context (81,986 tokens observed for MCP tools alone) |
| **Memory tool** | Still beta, file-based, mixed reviews |
| **Plan mode** | Described as "just a prompt" by multiple practitioners; sub-agent with zero visibility |
| **Plugins ecosystem** | Quality varies wildly; most add token overhead without proportional value |

### Key Data Points

- 90% of Claude Code's own code is written by Claude Code (Anthropic internal)
- ~1/3 of sessions still experience at least one flicker in the TUI (post differential renderer rewrite)
- Custom differential renderer shipped Jan 2026, reduced scroll events from 4000-6700/sec to near-zero
- v2.1 (January 2026): lazy MCP tool loading, improved context management, enhanced sub-agent background execution (Ctrl+B)

### Practitioner Consensus

"Less is more." Start with 2-3 essentials (CLAUDE.md, LSP, 1-2 MCPs), activate additional tools on-demand. The context window is the scarce resource -- every tool competes for it.

### Sources

- [Claude Code Must-Haves Jan 2026](https://dev.to/valgard/claude-code-must-haves-january-2026-kem)
- [Addy Osmani's LLM Workflow](https://addyosmani.com/blog/ai-coding-workflow/)
- [Anthropic Sandboxing](https://www.anthropic.com/engineering/claude-code-sandboxing)

---

## 4. Codex CLI (OpenAI)

### Architecture Evolution

Rebuilt from Node.js to **Rust** (97% of codebase). Authentication via ChatGPT Plus/Pro/Team or API key. Sandbox blocks network by default.

### Current Model

**GPT-5.3-Codex** (February 2026): Combines frontier coding performance of GPT-5.2-Codex with stronger reasoning, runs 25% faster. Available in Codex app, CLI, IDE extension, and Codex Cloud.

### Safety Levels

| Level       | Behavior         |
| ----------- | ---------------- |
| Read Only   | No writes        |
| Auto        | Sandboxed writes |
| Full Access | Unrestricted     |

Cross-platform sandbox: macOS Seatbelt, Linux Landlock.

### Recent Features (Late 2025 - Feb 2026)

| Feature                | Detail                                                             |
| ---------------------- | ------------------------------------------------------------------ |
| **Mid-turn steering**  | Submit messages while Codex is working to redirect                 |
| **Skills**             | Personal skills from `~/.agents/skills`, custom prompts deprecated |
| **Team Config**        | Standardize Codex across repos/machines via `.codex/` folders      |
| **Session resume**     | `codex resume` to continue where you left off                      |
| **Context compaction** | Auto-summarizes approaching context limit                          |
| **/plan command**      | Now accepts inline prompt + pasted images                          |
| **Parallel shell**     | Shell tools can run in parallel for throughput                     |
| **Git safety**         | Destructive/write git commands no longer bypass approval           |
| **Sub-agent limit**    | Reduced max sub-agents to 6 for resource guardrails                |
| **Desktop launch**     | `codex app <path>` on macOS                                        |
| **CI integration**     | Codex Autofix in GitHub Actions                                    |

### Performance Improvements

- GitHub disconnects reduced 90%
- PR creation latency reduced 35%
- Tool call latency reduced 50%
- Task completion latency reduced 20%

### Sources

- [Codex CLI Changelog](https://developers.openai.com/codex/changelog/)
- [Codex CLI Features](https://developers.openai.com/codex/cli/features/)
- [Codex GitHub](https://github.com/openai/codex)
- [Codex Releases](https://github.com/openai/codex/releases)

---

## 5. Gemini CLI (Google)

### Approach and Differentiators

| Aspect             | Detail                                                               |
| ------------------ | -------------------------------------------------------------------- |
| **Context window** | 1M tokens (Gemini 2.5 Pro) -- 10x larger than competitors            |
| **Free tier**      | 60 req/min, 1000/day via Google OAuth                                |
| **Model**          | Gemini 3 Pro available (Nov 2025)                                    |
| **License**        | Apache-2.0 (fully open source)                                       |
| **Stars**          | 93.7k                                                                |
| **Architecture**   | ReAct loop                                                           |
| **TUI**            | Alternate screen by default, prints transcript to scrollback on exit |

### Extension Ecosystem

Gemini CLI's key differentiator is its **extension system** -- MCP servers, custom commands, and markdown instructions packaged as distributable, installable extensions:

- Browse and install from extension marketplace
- Built-in support for Stdio, SSE, and Streamable HTTP MCP transports
- FastMCP integration for Python-based MCP servers
- Extensions include: Terraform, SonarQube, Genkit, Database toolbox (30+ datasources)
- Weekly release cadence (preview Tuesday, stable Tuesday)

### Notable Features

- Google Search grounding (built-in web search)
- Headless mode for scripting/automation
- Trusted folders for per-directory execution policies
- IDE integration (VS Code)
- Enterprise deployment via Vertex AI
- Sandboxing for safe execution

### Architecture Philosophy

Single ReAct loop (no sub-agents). The 1M token context window reduces the need for aggressive compaction or sub-agent context isolation. The alternate-screen TUI avoids all inline rendering challenges -- a pragmatic choice that sidesteps the flicker problems Claude Code and others face.

### Sources

- [Gemini CLI GitHub](https://github.com/google-gemini/gemini-cli)
- [Gemini CLI Docs](https://geminicli.com/docs/)
- [Gemini 3 Pro Announcement](https://github.com/google-gemini/gemini-cli/discussions/13280)
- [Gemini CLI Extensions](https://geminicli.com/extensions/)

---

## 6. Aider

### Still Relevant? Yes, for a Specific Niche

Aider occupies a distinct position: **git-native, diff-first AI coding**. It's not trying to be an autonomous agent -- it's a surgical editing tool that treats Git as the organizing principle.

### Core Architecture: Architect/Editor Split

Aider's key innovation is separating reasoning from editing:

1. **Architect** (reasoning model): Focuses on solving the coding problem, describes the solution naturally
2. **Editor** (editing model): Focuses on formatting the edits correctly

This two-phase approach produced SOTA results on Aider's code editing benchmark. The insight: when both tasks share a single prompt/response, the model splits attention between problem-solving and formatting. Separating them improves both.

### Git-Native Workflow

| Behavior                         | Detail                               |
| -------------------------------- | ------------------------------------ |
| Every edit commits automatically | Descriptive commit messages          |
| Dirty files committed first      | Separates user edits from AI edits   |
| Each interaction is atomic       | Diff, blame, revert via standard git |
| Repository map                   | Full codebase mapping for context    |

### Where Aider Fits in 2026

| Task                                   | Best Tool                                     |
| -------------------------------------- | --------------------------------------------- |
| Structured refactors, multi-file edits | **Aider** -- git-native, surgical, reviewable |
| Autonomous feature implementation      | Claude Code, Codex -- full agent loop         |
| Complex debugging, architecture        | Claude Code -- deep reasoning                 |
| Quick fixes, small tasks               | Any agent in rush/fast mode                   |

### Limitations

- Assumes terminal comfort and deliberate task framing
- Not autonomous -- requires explicit direction
- No sub-agents, no background execution
- Less capable for open-ended exploration

### Sources

- [Aider Architect/Editor](https://aider.chat/2024/09/26/architect.html)
- [Aider Git Integration](https://aider.chat/docs/git.html)
- [Aider Review](https://www.blott.com/blog/post/aider-review-a-developers-month-with-this-terminal-based-code-assistant)

---

## 7. Recursive Language Models (RLMs)

### What It Is

**NOT "Reinforcement Learning for LMs"** -- it stands for **Recursive Language Models**. Introduced by Prime Intellect (Alex Zhang, October 2025), paper arXiv:2512.24601.

RLMs are an architectural approach that enables language models to actively manage their own context through a Python REPL interface. Instead of processing large inputs directly, the model delegates work to sub-instances of itself.

### How It Works

```
Standard LLM Agent:
  [User Query] -> [LLM reads all context] -> [Tool calls] -> [All results into context] -> [Response]
  Problem: Context grows linearly with tool outputs

RLM:
  [User Query] -> [Root LLM with Python REPL] -> [Writes code to search/filter/transform data]
                                               -> [Spawns sub-LLMs via llm_batch() for parallel work]
                                               -> [Sub-LLMs return compressed results]
                                               -> [Root LLM synthesizes answer]
  Benefit: Main model context stays bounded
```

### Key Technical Components

1. **Python REPL access**: Persistent execution environment where the model can search, filter, transform data using standard libraries
2. **Sub-LLM delegation**: `llm_batch()` function spawns parallel sub-LLM calls for tool-intensive work (web searches, API calls, complex computations)
3. **Answer variable pattern**: Models write to an `answer` dictionary with `content` and `ready` keys, enabling iterative refinement across multiple reasoning turns
4. **Context as environment variable**: The entire prompt is treated as an external string that the LLM inspects/transforms through code, rather than ingesting all tokens into the Transformer context

### Benchmark Results (GPT-5 on CodeQA)

| Method              | Accuracy  |
| ------------------- | --------- |
| Base model (direct) | 24.00     |
| Summarization agent | 41.33     |
| **RLM**             | **62.00** |

On hardest setting (OOLONG Pairs): Base model F1 = 0.04, RLM F1 = 58.00.

### RL Integration

The framework is designed for RL training where models learn better chunking, recursion, and tool usage policies. Current experiments use API-based models, but the infrastructure supports end-to-end RL optimization. Prime Intellect's thesis: "Teaching models to manage their own context end-to-end through RL will be the next major breakthrough."

### Related Work

| Project             | Source             | Detail                                           |
| ------------------- | ------------------ | ------------------------------------------------ |
| **Agent-R1**        | USTC               | End-to-end RL for multi-turn agentic tasks       |
| **MAGRPO**          | Research           | Multi-agent cooperative RL for LLM collaboration |
| **Agent Lightning** | Microsoft Research | Drop-in RL for existing agents, no code rewrites |
| **TTRL**            | Open source        | Online RL without ground-truth labels            |

### Relevance to Coding Agents

A coding agent using RLM principles would:

1. Not dump entire file contents into context -- instead write code to search/grep relevant sections
2. Delegate sub-tasks (research, testing, review) to sub-LLM instances with focused context
3. Learn optimal context management strategies through RL (which files matter, how much to read, when to delegate)
4. Handle 10M+ token codebases by recursively delegating rather than trying to fit everything in context

### Sources

- [Prime Intellect RLM Blog](https://www.primeintellect.ai/blog/rlm)
- [MarkTechPost on RLMs](https://www.marktechpost.com/2026/01/02/recursive-language-models-rlms-from-mits-blueprint-to-prime-intellects-rlmenv-for-long-horizon-llm-agents/)
- [Agent-R1 Paper](https://arxiv.org/html/2511.14460v1)
- [Tsinghua RL for LRMs Survey](https://github.com/TsinghuaC3I/Awesome-RL-for-LRMs)
- [Agent Lightning](https://www.microsoft.com/en-us/research/blog/agent-lightning-adding-reinforcement-learning-to-ai-agents-without-code-rewrites/)

---

## 8. Emerging Patterns Across Coding Agents

### What's Being Adopted

| Pattern                         | Evidence                                                       | Agents                             |
| ------------------------------- | -------------------------------------------------------------- | ---------------------------------- |
| **Skills over custom commands** | Reusable instruction packages, shared like npm modules         | Amp, Claude Code, Codex, Crush     |
| **Aggressive feature removal**  | Remove anything that doesn't justify its token/complexity cost | Amp, Pi-Mono                       |
| **Minimal system prompts**      | Frontier models already know how to be coding agents           | Pi-Mono (<1k tokens)               |
| **Context as scarce resource**  | Every tool competes for context window; lazy loading preferred | All                                |
| **Git safety hardening**        | Destructive git commands require explicit approval             | Codex, Claude Code                 |
| **Mid-turn steering**           | Redirect agent while it's working, don't wait for completion   | Codex, Claude Code                 |
| **Session resume/continuity**   | Pick up where you left off across sessions                     | Codex, OpenCode, Claude Code       |
| **Team/project config layers**  | Hierarchical config (project > user > system)                  | Codex, Claude Code, Pi-Mono, Crush |
| **Architect/Editor separation** | Split reasoning from formatting for better results             | Aider (pioneered), others adopting |
| **Ralph Wiggum loops**          | Autonomous agent loops until success criteria met              | Claude Code Tasks, custom setups   |
| **Multi-agent orchestration**   | Parallel agents with isolated context                          | Claude Code, Codex, OpenCode, Amp  |

### What's Being Dropped

| Pattern                        | Why                                                              | Evidence                             |
| ------------------------------ | ---------------------------------------------------------------- | ------------------------------------ |
| **Tab/inline completion**      | "Post-agentic age" -- agents write whole features, not lines     | Amp removed Tab                      |
| **Built-in plan mode**         | Just a prompt; sub-agent with zero visibility; ask instead       | Amp removed, Pi never had it         |
| **TODO/task lists**            | Models track work implicitly; explicit lists waste tokens        | Amp removed TODOs                    |
| **Custom commands**            | Redundant with skills                                            | Amp, Codex deprecated custom prompts |
| **BYOK/isolated mode**         | Slows model-product co-evolution                                 | Amp removed                          |
| **Excessive MCP loading**      | Token overhead without proportional value; 40%+ context consumed | Practitioner consensus               |
| **Full-screen TUI**            | Codex abandoned fullscreen TUI2; terminal-native scrollback won  | Codex, Claude Code, Pi-Mono          |
| **Complex permission systems** | Models are safe enough; YOLO is default for experienced users    | Pi-Mono, Amp                         |

### What's Being Added

| Pattern                         | Why                                              | Evidence                           |
| ------------------------------- | ------------------------------------------------ | ---------------------------------- |
| **LSP integration**             | Semantic code understanding >> text file parsing | OpenCode, Crush, Claude Code       |
| **Extension/plugin ecosystems** | Community-driven tool distribution               | Gemini CLI, Claude Code, Codex     |
| **Desktop apps alongside CLI**  | Different users, same backend                    | OpenCode, Codex                    |
| **CI/CD integration**           | Agents in automated pipelines                    | Codex Autofix, GitHub Actions      |
| **Shareable sessions**          | Collaboration, debugging, teaching               | OpenCode, Amp                      |
| **Image/screenshot tools**      | Visual debugging and design work                 | Amp (Painter), Codex (image paste) |

### The Convergent Architecture

Every successful coding agent has converged on essentially the same core:

```
1. Agent loop (while model produces tool calls)
2. File tools (read, write, edit)
3. Shell tool (bash)
4. Search tools (glob, grep)
5. Project context file (CLAUDE.md / AGENTS.md / GEMINI.md)
6. Session persistence
7. Context management (compaction or large window)
```

Differentiation happens above this base:

- **Memory**: No agent has solved persistent semantic memory (biggest gap)
- **Context efficiency**: Pi-Mono's minimalism vs. Claude Code's feature richness
- **Provider flexibility**: OpenCode (75+) vs. Claude Code (Anthropic only)
- **TUI quality**: OpenTUI, Charm ecosystem, custom crossterm
- **Sub-agent strategy**: Full isolation (Claude Code) vs. none (Pi-Mono, Gemini CLI)

### Anthropic's 2026 Trends Report Key Findings

From Anthropic's official report (January 2026):

- Engineers shifting from writing code to **orchestrating agents**
- AI used in 60% of workflows, but only 0-20% fully delegated
- 57% of organizations deploy multi-step agent workflows
- Developers delegate tasks that are **easily verifiable** or **low-stakes**
- Rise of "papercut fixing" -- agents clearing years of minor bugs/tech debt at near-zero cost
- Security becoming easier (any engineer can leverage AI for security reviews)

### Sources

- [Anthropic 2026 Trends Report](https://resources.anthropic.com/hubfs/2026%20Agentic%20Coding%20Trends%20Report.pdf)
- [Addy Osmani 2026 Trends](https://beyond.addy.ie/2026-trends/)
- [Addy Osmani LLM Workflow](https://addyosmani.com/blog/ai-coding-workflow/)

---

## Summary Table

| Agent           | Language | Stars | Tools                    | Sub-Agents           | MCP           | Philosophy               |
| --------------- | -------- | ----- | ------------------------ | -------------------- | ------------- | ------------------------ |
| **Amp**         | TS       | N/A   | Skills-based             | Yes (oracle, panels) | No            | Aggressive subtraction   |
| **Pi-Mono**     | TS       | 1.9k  | 4 (read/write/edit/bash) | None (by design)     | None          | Extreme minimalism       |
| **OpenCode**    | TS       | 99.4k | LSP-enhanced             | 3 built-in           | Client        | Maximum flexibility      |
| **Claude Code** | TS       | 57.6k | Full suite               | Yes (Task tool)      | Client+Server | Feature-rich, sandboxed  |
| **Codex CLI**   | Rust     | 56.3k | Full suite + skills      | Yes (limited to 6)   | Client        | Rust performance, safety |
| **Gemini CLI**  | TS       | 93.7k | Extension ecosystem      | None                 | Client        | 1M context, extensions   |
| **Aider**       | Python   | N/A   | Git-native editing       | None                 | None          | Surgical, diff-first     |

---

## Implications for Ion

### Validated Decisions

1. **Minimal tool set is correct** -- Pi-Mono and Terminus 2 prove 4 tools + bash is competitive
2. **Custom TUI over ratatui** -- Codex abandoned fullscreen TUI2, every successful agent uses custom rendering
3. **AGENTS.md support** -- universal standard across all agents
4. **Context efficiency matters** -- MCP overhead is a real problem practitioners complain about

### Opportunities

1. **Memory remains the primary gap** -- no agent has persistent semantic memory
2. **RLM-style context management** -- delegating context to sub-processes rather than ingesting everything could be a differentiator
3. **LSP integration** -- moving from "table stakes" to "essential" based on practitioner feedback
4. **Session resume** -- increasingly expected feature

### Anti-Patterns to Avoid

1. Excessive MCP tool loading at startup
2. Built-in plan mode (just let users ask for plans)
3. Complex permission systems for experienced users (YOLO default)
4. Full-screen TUI ownership
5. Feature accumulation without proportional value

---

## Change Log

- 2026-02-09: Initial comprehensive research across 8 topics
