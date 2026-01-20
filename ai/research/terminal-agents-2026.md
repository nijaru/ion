# Terminal-Based Coding Agents: State of the Art (January 2026)

**Research Date**: 2026-01-17
**Purpose**: Compare leading terminal AI coding agents for architecture, memory, sub-agents, and differentiation
**Scope**: Claude Code, Gemini CLI, Codex CLI, OpenCode, Pi-Mono, Crush

---

## Executive Summary

| Agent           | Language   | Stars | Memory      | Sub-Agents       | MCP Support      | Key Differentiator            |
| --------------- | ---------- | ----- | ----------- | ---------------- | ---------------- | ----------------------------- |
| **Claude Code** | TypeScript | 57.6k | None (beta) | Yes (Task tool)  | Client + Server  | Sandboxing, Skills, dual MCP  |
| **Gemini CLI**  | TypeScript | 90.6k | None        | No               | Client           | 1M context, free tier, Google |
| **Codex CLI**   | Rust       | 56.3k | None        | Yes (Review)     | Client           | Rust core, ChatGPT Plus       |
| **OpenCode**    | TypeScript | 75k   | None        | Yes (general)    | Client           | 75+ providers, LSP, sessions  |
| **Pi-Mono**     | TypeScript | 1.9k  | None        | None (by design) | None (by design) | Minimal context, CLI tools    |
| **Crush**       | Go         | 18.2k | None        | No               | Client           | Glamorous TUI, Skills, LSP    |

**Key Insight**: None of these agents have persistent semantic memory. This remains the primary differentiation opportunity for ion.

---

## 1. Claude Code (Anthropic)

**Repository**: https://github.com/anthropics/claude-code
**Language**: TypeScript
**Stars**: 57.6k
**License**: Proprietary

### Architecture

**Core Loop**: Classic agent loop with intentional simplicity

```
while(tool_call):
    execute_tool()
    feed_results_back()
    repeat()
```

**Design Principles**:

- Single main thread with flat message list
- No swarms or competing agents
- Debuggability over complexity
- Subagents cannot spawn other subagents (prevents infinite nesting)

### Memory/Context Management

| Aspect                  | Implementation                                     |
| ----------------------- | -------------------------------------------------- |
| **Persistent Memory**   | None (Memory tool in beta - file-based)            |
| **Context Compaction**  | Auto-summarization near context limit              |
| **Compaction Strategy** | Preserve decisions, discard redundant tool outputs |
| **Sub-Agent Isolation** | Each sub-agent has separate context window         |

**v2.1 Updates (January 2026)**:

- Lazy MCP tool loading (only loads tools when needed)
- Improved context management
- Enhanced sub-agent background execution (Ctrl+B)

### Sub-Agent Architecture

**Built-In Agents**:
| Agent | Model | Tools | Purpose |
|-------|-------|-------|---------|
| Explore | Haiku | Read-only | Fast codebase search |
| Plan | Inherits | Read-only | Research for planning |
| General | Inherits | All | Complex multi-step tasks |

**Custom Agents**: Defined in `.claude/agents/*.prompt.md`

```markdown
---
description: Code review specialist
tools: [Read, Grep, Glob]
disallowedTools: [Write, Edit]
model: claude-haiku-4-20250514
---

You are a code reviewer...
```

### Tool Permission Model

| Level              | Description                                      |
| ------------------ | ------------------------------------------------ |
| **Sandboxing**     | 84% reduction in permission prompts              |
| **Filesystem**     | Network isolation via unix domain socket         |
| **Proxy**          | Domain filtering outside sandbox                 |
| **Auto-Decisions** | Allow safe ops, block malicious, ask when needed |

### Key Differentiators

1. **Dual MCP Role**: Acts as both MCP client AND server
2. **Skills System**: SKILL.md format (progressive disclosure: ~50 tokens at startup, full load on demand)
3. **Sandboxing**: Industry-leading permission reduction
4. **Checkpoints**: State save/restore capabilities
5. **Deep Integration**: Optimized for Claude 4/4.5 models

---

## 2. Gemini CLI (Google)

**Repository**: https://github.com/google-gemini/gemini-cli
**Language**: TypeScript
**Stars**: 90.6k
**License**: Apache-2.0

### Architecture

**Core Pattern**: ReAct (Reason and Act) loop

- Uses built-in tools and MCP servers
- Google Search grounding built-in
- Sandboxing for safe execution

**Components**:

- CLI package (`packages/cli`) - User-facing interface
- Agent package - ReAct loop implementation
- Tool registry - Built-in + MCP tools

### Memory/Context Management

| Aspect                | Implementation                     |
| --------------------- | ---------------------------------- |
| **Context Window**    | 1M tokens (Gemini 2.5 Pro)         |
| **Persistent Memory** | None                               |
| **Compaction**        | Conversation checkpointing         |
| **Project Context**   | GEMINI.md for project instructions |

**Largest Context Window**: 1M tokens is the highest among all agents, reducing need for aggressive compaction.

### Sub-Agent Architecture

**No built-in sub-agents**. Uses single ReAct loop.

### Tool Permission Model

| Feature             | Implementation                             |
| ------------------- | ------------------------------------------ |
| **Sandboxing**      | Yes, for safe execution                    |
| **Trusted Folders** | Configure execution policies per directory |
| **MCP Integration** | Full client support                        |

### Authentication Options

| Method       | Limits                    |
| ------------ | ------------------------- |
| Google OAuth | 60 req/min, 1000/day free |
| API Key      | 100 req/day free          |
| Vertex AI    | Enterprise (pay-per-use)  |

### Key Differentiators

1. **Free Tier**: Generous limits without API costs
2. **1M Token Context**: Largest context window
3. **Google Search Grounding**: Native web search integration
4. **Enterprise Ready**: Vertex AI integration
5. **Open Source**: Apache-2.0 license

---

## 3. Codex CLI (OpenAI)

**Repository**: https://github.com/openai/codex
**Language**: Rust (97%)
**Stars**: 56.3k
**License**: Apache-2.0

### Architecture

**Core Pattern**: Task-based workflow with multi-turn completion

```
run_task() -> Loop {
  1. Get pending user input
  2. Build conversation history
  3. run_turn() -> stream from model
  4. Process responses:
     - Empty -> break (task complete)
     - Token limit -> auto-compact & retry
  5. Loop back
}
```

**Key Insight**: Continues until model stops producing responses (not fixed iterations).

### Memory/Context Management

| Aspect                 | Implementation                          |
| ---------------------- | --------------------------------------- |
| **Session State**      | Full conversation history + token usage |
| **Auto-Compaction**    | Triggers at configurable threshold      |
| **Compaction Process** | Summarize + truncate oldest items       |
| **Token Tracking**     | Real-time updates after each turn       |

**OpenAI Agents SDK (2026)**: New `RunContextWrapper` provides:

- Structured state objects that persist across runs
- Memory, notes, preferences that evolve
- Context-injection logic for personalization

### Sub-Agent Architecture

**Task Types**:
| Task | Purpose |
| ---- | ------- |
| RegularTask | Standard chat/coding |
| ReviewTask | Structured code review sub-agent |
| CompactTask | Conversation summarization |
| UndoTask | Revert changes |
| GhostSnapshotTask | Parallel commit tracking |

**Review Sub-Agent**:

- Separate conversation with REVIEW_PROMPT
- Structured output with priority levels (P0-P3)
- JSON schema for findings

### Tool Permission Model

**Three-Stage Dispatch**:

1. Build ToolCall from ResponseItem
2. ToolRegistry routes to handler
3. Failure handling with recovery

**ToolOrchestrator Pattern**:

1. **Approval Phase**: Check if tool needs permission
2. **Execution Phase**: Sandbox -> Execute -> Handle failure
3. **Escalation**: Re-approval for unboxed retry

**Approval Caching**: Only ask once per session, escalate on failure.

### Key Differentiators

1. **Rust Core**: High performance, 97% Rust codebase
2. **ChatGPT Integration**: Use with Plus/Pro subscription
3. **Multi-Turn Loop**: Natural task completion
4. **Structured Review**: Priority-based code review
5. **Parallel Execution**: FuturesOrdered for tool calls

---

## 4. OpenCode (Anomaly)

**Repository**: https://github.com/anomalyco/opencode
**Website**: https://opencode.ai
**Language**: TypeScript
**Stars**: 75k
**License**: MIT

### Architecture

**Core Pattern**: Client/Server architecture

- TUI frontend (one of multiple possible clients)
- Backend can run locally while driven remotely (mobile app)
- Built by neovim users and terminal.shop creators

**Deployment Options**:

- Terminal TUI
- Desktop app (macOS, Windows, Linux)
- IDE extension

### Memory/Context Management

| Aspect                | Implementation                  |
| --------------------- | ------------------------------- |
| **Persistent Memory** | None                            |
| **Auto-Compaction**   | At 95% context window           |
| **Multi-Session**     | Parallel agents on same project |
| **Share Links**       | Shareable session URLs          |

### Sub-Agent Architecture

**Built-In Agents**:
| Agent | Purpose | Restrictions |
| ----- | ------- | ------------ |
| **build** | Default, full access | None |
| **plan** | Read-only analysis | Denies edits, asks for bash |
| **general** | Complex searches | Invoke via @general |

### Tool Permission Model

| Feature             | Implementation                        |
| ------------------- | ------------------------------------- |
| **LSP Integration** | Automatically loads appropriate LSPs  |
| **MCP Client**      | Full support for external tools       |
| **Provider Login**  | Claude Pro, ChatGPT Plus/Pro accounts |

### Provider Support

**75+ LLM Providers** via Models.dev:

- Anthropic (Claude Pro/Max)
- OpenAI (ChatGPT Plus/Pro)
- Google Gemini
- Local models
- OpenRouter and more

### Key Differentiators

1. **Maximum Flexibility**: 75+ providers, any model
2. **LSP Integration**: Semantic code understanding
3. **Multi-Session**: Parallel execution on same project
4. **Client/Server**: Remote control from mobile
5. **100% Open Source**: MIT license, community-driven

---

## 5. Pi-Mono (badlogic/Mario Zechner)

**Repository**: https://github.com/badlogic/pi-mono
**Website**: https://shittycodingagent.ai
**Language**: TypeScript (npm workspaces monorepo)
**Stars**: 1.9k
**License**: MIT

### Architecture

**Philosophy**: "If I don't need it, won't build it"

**Monorepo Structure**:

```
pi-mono/
  packages/
    pi-ai/           # Unified LLM API
    pi-agent-core/   # Agent loop, state
    pi-tui/          # Differential TUI
    pi-coding-agent/ # CLI
    pi-mom/          # Slack bot
    pi-web-ui/       # Web components
    pi-pods/         # vLLM deployment
```

**Build System**: npm workspaces + TypeScript ESM + Biome + tsx

### Memory/Context Management

| Aspect                 | Implementation                   |
| ---------------------- | -------------------------------- |
| **Persistent Memory**  | None                             |
| **Compaction**         | Planned (issue #92)              |
| **Session Management** | Continue, resume, branching      |
| **Context Cost**       | <1000 tokens total system prompt |

### Sub-Agent Architecture

**None by Design**

> "When Claude Code spawns a sub-agent, you have zero visibility. It's a black box within a black box."

**Alternative**: Spawn Pi instances via bash/tmux for full observability

```bash
pi --prompt "Review this PR" --model claude-sonnet
tmux new-session pi --prompt "Research the codebase"
```

### Tool Permission Model

**Minimal (4 Core Tools)**:
| Tool | Purpose |
| ---- | ------- |
| bash | Shell execution |
| read | File reading |
| write | File writing |
| edit | String replacement |

**YOLO Mode**: Default, minimal permission prompts

> "All frontier models have been RL-trained up the wazoo, so they inherently understand what a coding agent is. There does not appear to be a need for 10,000 tokens of system prompt."

### Why No MCP

> "Popular MCP servers like Playwright MCP (21 tools, 13.7k tokens) dump their entire tool descriptions into your context on every session. That's 7-9% of your context window gone."

**Alternative**: CLI tools with READMEs (progressive disclosure)

- Token cost only when needed
- Composable (pipe outputs, chain commands)
- Tool collection: github.com/badlogic/agent-tools

### Key Differentiators

1. **Extreme Minimalism**: 4 tools, <1000 token system prompt
2. **Context Efficiency**: No MCP overhead
3. **Full Observability**: No black-box sub-agents
4. **Differential TUI**: Efficient terminal rendering
5. **15+ Direct Providers**: Anthropic, OpenAI, Google, Mistral, Groq, etc.

---

## 6. Crush (Charmbracelet)

**Repository**: https://github.com/charmbracelet/crush
**Website**: https://charm.land/crush
**Language**: Go
**Stars**: 18.2k
**License**: Proprietary (CLA required)

### Architecture

**Built on Charm Ecosystem**: Bubble Tea, Lip Gloss, etc. (powering 25k+ applications)

**Core Features**:

- Multi-model with mid-session switching
- Session-based with multiple contexts per project
- LSP-enhanced context
- MCP extensible (http, stdio, sse)

### Memory/Context Management

| Aspect                | Implementation                             |
| --------------------- | ------------------------------------------ |
| **Persistent Memory** | None                                       |
| **Sessions**          | Multiple per project, context preserved    |
| **Model Switching**   | Mid-session with context preservation      |
| **Provider Updates**  | Auto-updates from Catwalk (community repo) |

### Sub-Agent Architecture

**No built-in sub-agents**. Single agent loop with LSP enhancement.

### Tool Permission Model

| Feature            | Implementation                |
| ------------------ | ----------------------------- |
| **Default**        | Ask before tool calls         |
| **Allowed Tools**  | Configure in crush.json       |
| **YOLO Flag**      | `--yolo` skips all prompts    |
| **Disabled Tools** | `options.disabled_tools` list |

### Configuration

**Hierarchy**:

1. `.crush.json` (local)
2. `crush.json` (local)
3. `$HOME/.config/crush/crush.json` (global)

**MCP Support**:

```json
{
  "mcps": {
    "github": {
      "type": "stdio",
      "command": "github-mcp-server"
    }
  }
}
```

**Skills Support**:

- Discovers from `~/.config/crush/skills/`
- SKILL.md format (Agent Skills standard)
- Compatible with anthropics/skills

### Provider Support

| Provider       | Environment Variable        |
| -------------- | --------------------------- |
| Anthropic      | ANTHROPIC_API_KEY           |
| OpenAI         | OPENAI_API_KEY              |
| OpenRouter     | OPENROUTER_API_KEY          |
| Google Gemini  | GEMINI_API_KEY              |
| Vertex AI      | VERTEXAI_PROJECT + LOCATION |
| Amazon Bedrock | AWS credentials             |
| Azure OpenAI   | AZURE*OPENAI*\*             |
| Groq           | GROQ_API_KEY                |
| Cerebras       | CEREBRAS_API_KEY            |
| Hugging Face   | HF_TOKEN                    |

**Local Models**: Ollama, LM Studio via OpenAI-compat API

### Key Differentiators

1. **Glamorous TUI**: Beautiful terminal interface (Charm ecosystem)
2. **LSP-Enhanced**: Uses LSPs like developers do
3. **Skills Standard**: agentskills.io compatibility
4. **Multi-Platform**: macOS, Linux, Windows, Android, \*BSD
5. **Catwalk**: Community-managed provider/model database

---

## Comparative Analysis

### Memory Systems

| Agent       | Episodic | Semantic | Knowledge Graph | Cross-Session     |
| ----------- | -------- | -------- | --------------- | ----------------- |
| Claude Code | None     | None     | None            | File-based (beta) |
| Gemini CLI  | None     | None     | None            | None              |
| Codex CLI   | None     | None     | None            | Session history   |
| OpenCode    | None     | None     | None            | Share links       |
| Pi-Mono     | None     | None     | None            | Session files     |
| Crush       | None     | None     | None            | Session files     |

**Conclusion**: Memory remains completely unexplored. This is ion's differentiation opportunity.

### Context Management Approaches

| Agent       | Strategy             | Threshold    | Innovation          |
| ----------- | -------------------- | ------------ | ------------------- |
| Claude Code | Auto-summarize       | ~80%         | Sub-agent isolation |
| Gemini CLI  | Checkpoint           | 1M tokens    | Largest window      |
| Codex CLI   | Auto-compact + retry | Configurable | Real-time tracking  |
| OpenCode    | Auto-compact         | 95%          | Multi-session       |
| Pi-Mono     | Planned              | N/A          | Minimal footprint   |
| Crush       | Session-based        | Per-session  | Model switching     |

### Sub-Agent Patterns

| Agent       | Pattern               | Isolation     | Recursion  |
| ----------- | --------------------- | ------------- | ---------- |
| Claude Code | Task tool             | Full context  | Blocked    |
| Gemini CLI  | None                  | N/A           | N/A        |
| Codex CLI   | Task types            | Separate conv | Blocked    |
| OpenCode    | Built-in agents       | Shared        | Tab switch |
| Pi-Mono     | None (spawn via bash) | Full          | Manual     |
| Crush       | None                  | N/A           | N/A        |

### Tool Permission Models

| Agent       | Default         | Caching      | Escalation |
| ----------- | --------------- | ------------ | ---------- |
| Claude Code | Sandbox         | Yes          | Yes        |
| Gemini CLI  | Trusted folders | Yes          | No         |
| Codex CLI   | Approval        | Session      | Yes        |
| OpenCode    | Per-agent       | No           | No         |
| Pi-Mono     | YOLO            | N/A          | N/A        |
| Crush       | Ask             | Configurable | No         |

---

## Feature Classification

### Table Stakes (Must Have)

| Feature             | Status Across Agents      |
| ------------------- | ------------------------- |
| File operations     | All have read/write/edit  |
| Shell execution     | All support bash          |
| Multi-provider      | Most (except Claude Code) |
| Session persistence | All support               |
| Context management  | All have some form        |
| AGENTS.md/CLAUDE.md | Most support              |
| Skills/SKILL.md     | Claude, Pi, Crush, Codex  |

### Competitive Differentiators

| Feature              | Best Implementation     | Notes                  |
| -------------------- | ----------------------- | ---------------------- |
| MCP                  | Claude Code (dual role) | Client AND server      |
| Context window       | Gemini CLI (1M)         | 10x larger than others |
| Provider flexibility | OpenCode (75+)          | Maximum choice         |
| Context efficiency   | Pi-Mono (<1k tokens)    | Minimal overhead       |
| TUI quality          | Crush (Charm)           | Industrial-grade       |
| Performance          | Codex (Rust)            | Native speed           |

### Unexplored Opportunities

| Opportunity               | Current Gap  | ion Approach                  |
| ------------------------- | ------------ | ----------------------------- |
| **Semantic Memory**       | None have it | OmenDB vectors                |
| **Budget-Aware Assembly** | None         | ACE scoring + RRF             |
| **Knowledge Graph**       | None         | Entity extraction             |
| **Query Classification**  | None         | Skip memory for transactional |
| **Time Decay**            | None         | Proactive pruning             |

---

## Recommendations for ion

### Architecture Decision

Based on this research, ion should:

1. **Match Table Stakes**: File ops, shell, multi-provider, sessions, compaction, AGENTS.md, Skills
2. **Differentiate on Memory**: Budget-aware context assembly is novel and valuable
3. **Keep Context Lean**: Learn from Pi-Mono's minimalism
4. **Support MCP**: Client first (ecosystem compatibility)
5. **Consider LSP**: OpenCode and Crush show value of LSP enhancement

### Priority Features

| Priority | Feature            | Source          | Why                    |
| -------- | ------------------ | --------------- | ---------------------- |
| **P0**   | Memory system      | Novel           | Primary differentiator |
| **P0**   | Multi-provider     | OpenCode        | User expectation       |
| **P1**   | Skills (SKILL.md)  | Claude, Pi      | Standard format        |
| **P1**   | MCP client         | All except Pi   | Ecosystem              |
| **P1**   | Context compaction | All             | Long sessions          |
| **P2**   | Sub-agents         | Claude, Codex   | Complex tasks          |
| **P2**   | LSP integration    | OpenCode, Crush | Semantic context       |
| **P3**   | Sandboxing         | Claude          | Permission reduction   |

### Memory as Differentiator

| What Others Do          | What ion Does                 |
| ----------------------- | ----------------------------- |
| Regex search (Claude)   | Hybrid BM25 + vector          |
| Session history only    | Episodic + semantic + working |
| No relevance scoring    | ACE counters + time decay     |
| Full context injection  | Budget-aware assembly         |
| No query classification | Skip memory for transactional |

---

## References

**Claude Code**:

- https://github.com/anthropics/claude-code
- https://docs.anthropic.com/en/docs/claude-code
- https://www.anthropic.com/engineering/claude-code-sandboxing

**Gemini CLI**:

- https://github.com/google-gemini/gemini-cli
- https://geminicli.com/docs/
- https://docs.cloud.google.com/gemini/docs/codeassist/gemini-cli

**Codex CLI**:

- https://github.com/openai/codex
- https://developers.openai.com/codex
- https://cookbook.openai.com/examples/agents_sdk/context_personalization

**OpenCode**:

- https://github.com/anomalyco/opencode
- https://opencode.ai/docs

**Pi-Mono**:

- https://github.com/badlogic/pi-mono
- https://mariozechner.at/posts/2025-11-30-pi-coding-agent/
- https://deepwiki.com/badlogic/pi-mono

**Crush**:

- https://github.com/charmbracelet/crush
- https://charm.land/crush
- https://github.com/charmbracelet/catwalk

---

## Change Log

- 2026-01-17: Initial comprehensive research from multiple sources
