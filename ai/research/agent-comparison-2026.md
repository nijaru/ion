# Agent Architecture Comparison: Pi-Mono vs Claude Code

**Research Date**: 2026-01-16
**Purpose**: Inform ion architecture with lessons from minimalist (Pi-Mono) and sophisticated (Claude Code) agent designs
**Scope**: Tool sets, context management, sub-agent patterns, memory/persistence, design philosophy

---

## Executive Summary

| Aspect            | Pi-Mono                              | Claude Code                     |
| ----------------- | ------------------------------------ | ------------------------------- |
| **Philosophy**    | "If I don't need it, won't build it" | "Simplicity through constraint" |
| **Tools**         | 4 core (bash, read, write, edit)     | 6+ core + extensive guidance    |
| **System Prompt** | <1000 tokens                         | ~10,000 tokens                  |
| **Sub-agents**    | None (use tmux/bash)                 | Full Task tool + custom agents  |
| **MCP**           | None (CLI + README alternative)      | Full client + server            |
| **Memory**        | None                                 | None (opportunity for ion)      |
| **Context Mgmt**  | Manual/planned compaction            | Auto-compaction + isolation     |

**Key Insight**: Both succeed through constraint, but at different levels. Pi chooses extreme minimalism (4 tools, no sub-agents), Claude Code chooses bounded complexity (rich features with guardrails). Memory remains unexplored by both.

---

## Pi-Mono (badlogic/pi-mono)

### Philosophy: Radical Minimalism

**Core Principle**: "My philosophy in all of this was: if I don't need it, it won't be built. And I don't need a lot of things."

**Creator**: Mario Zechner (badlogic) - built pi because "Claude Code has turned into a spaceship with 80% of functionality I have no use for"

**Design Decisions**:

- No features added without proven need
- Context efficiency over capability breadth
- Full observability over black-box convenience
- CLI tools over protocols

### Architecture

**Monorepo Structure** (TypeScript):

```
pi-mono/
  packages/
    pi-ai/           # Unified LLM API (Anthropic, OpenAI, Google, etc.)
    pi-agent-core/   # Agent loop, state management
    pi-tui/          # Differential terminal rendering
    pi-coding-agent/ # CLI that wires it together
    pi-mom/          # Slack bot (reuses coding-agent)
    pi-web-ui/       # Web components
    pi-pods/         # vLLM deployment
```

**Dependency Flow**:

```
Tier 1 (Core): pi-ai, pi-tui (no inter-dependencies)
       |
       v
Tier 2 (Logic): pi-agent-core (depends on pi-ai, pi-tui)
       |
       v
Tier 3 (Apps): pi-coding-agent, pi-mom, pi-pods
```

### Tool Set: The Minimal Four

**Default Tools** (all you need):

```typescript
// Total system prompt + tools: <1000 tokens
const tools = [
  "bash", // Shell command execution
  "read", // File reading
  "write", // File writing
  "edit", // File editing (string replacement)
];
```

**Read-Only Mode Tools** (when restricting agent):

- `grep` - Pattern search
- `find` - File discovery
- `ls` - Directory listing

**Why Only Four?**

- "Models know how to use bash and have been trained on read, write, edit with similar input schemas"
- "These four tools are all you need for an effective coding agent"
- Codex CLI uses similarly minimal tools
- Frontier models are "RL-trained up the wazoo" and inherently understand coding agents

**Comparison with Claude Code**:
| Tool | Pi-Mono | Claude Code |
|------|---------|-------------|
| Shell | bash | Bash (+ detailed guidance) |
| Read | read | Read |
| Write | write | Write |
| Edit | edit | Edit (string replacement) |
| Search | (via bash grep) | GrepTool (dedicated) |
| Glob | (via bash find) | GlobTool (dedicated) |
| Web | (via CLI tools) | WebFetch, WebSearch |
| Task | None | Task (sub-agents) |

### System Prompt: Extreme Minimalism

**Pi's Full System Prompt**:

```
You are a coding assistant. Help the user with their coding tasks.
[AGENTS.md content injected here]
```

**That's it.** Compare to Claude Code's ~10,000 token system prompt with:

- Detailed tool usage examples
- Git commit message guidelines
- PR creation workflows
- Error handling patterns
- Security rules

**Why This Works**:

> "All the frontier models have been RL-trained up the wazoo, so they inherently understand what a coding agent is. There does not appear to be a need for 10,000 tokens of system prompt."

**Benchmark Validation**: Pi performs competitively on Terminal-Bench despite minimal prompting.

### No MCP (By Design)

**The Problem with MCP**:

> "Popular MCP servers like Playwright MCP (21 tools, 13.7k tokens) or Chrome DevTools MCP (26 tools, 18k tokens) dump their entire tool descriptions into your context on every session. That's 7-9% of your context window gone before you even start working."

**The Alternative: CLI Tools + READMEs**:

```bash
# Example: Adding web search to Pi
# 1. Create CLI tool with README
# 2. Agent reads README on demand (progressive disclosure)
# 3. Invoke via bash

# Benefits:
# - Token cost only when needed
# - Composable (pipe outputs, chain commands)
# - Easy to extend (just add another script)
```

**Tool Collection**: [github.com/badlogic/agent-tools](https://github.com/badlogic/agent-tools)

**For MCP Compatibility**: Use [mcporter](https://github.com/steipete/mcporter) to wrap MCP servers as CLI tools

### No Sub-Agents (Observability Over Convenience)

**The Problem with Sub-Agents**:

> "When Claude Code needs to do something complex, it often spawns a sub-agent to handle part of the task. You have zero visibility into what that sub-agent does. It's a black box within a black box."

**Issues**:

- Context transfer between agents is poor
- Orchestrator decides what context to pass
- If sub-agent makes a mistake, debugging is painful
- Can't see the full conversation

**The Alternative: Spawn Pi via Bash**:

```bash
# For code review
pi --prompt "Review this PR" --model claude-sonnet

# For full observability, spawn in tmux
tmux new-session pi --prompt "Research the codebase"
```

**Workflow Philosophy**:

> "Using a sub-agent mid-session for context gathering is a sign you didn't plan ahead. If you need to gather context, do that first in its own session. Create an artifact that you can later use in a fresh session."

**Valid Use Cases for Sub-Agents**:

- Code review (via custom slash command)
- Ephemeral research tasks
- NOT parallel feature implementation ("anti-pattern, doesn't work")

### Context Management

**Compaction** (Planned Feature):

- Issue #92 tracks implementation
- Will summarize older messages while keeping recent ones
- Not yet implemented, but author reports able to do "hundreds of exchanges" in single session without it

**Session Management**:

- Continue, resume, branching
- HTML export of sessions
- Full cost and token tracking
- Message queuing while agent works

### Benchmark Results

**Terminal-Bench 2.0** (from author's testing):

- Pi performs competitively with Claude Code, Codex, Cursor
- Minimal tooling does not hurt performance
- Evidence that "simpler is sufficient"

**Terminus 2 Reference**:

> "Terminus 2 is a minimal agent that just gives the model a tmux session. The model sends commands as text to tmux and parses the terminal output itself. No fancy tools, no file operations, just raw terminal interaction. And it's holding its own against agents with far more sophisticated tooling."

### What Pi Deliberately Omits

| Feature           | Pi Status | Rationale                        |
| ----------------- | --------- | -------------------------------- |
| MCP               | No        | Context overhead, use CLI tools  |
| Sub-agents        | No        | Black box, poor context transfer |
| Plan mode         | No        | Write plans to file, start fresh |
| Permission popups | Minimal   | "YOLO mode", security theater    |
| Background bash   | No        | Explicit control preferred       |
| Built-in to-dos   | No        | Use markdown files               |
| Max steps         | No        | "Loop until agent says done"     |

---

## Claude Code (Anthropic)

### Philosophy: Simplicity Through Constraint

**Core Principle**: "At Claude Code's heart beats a classic agent loop that embodies simplicity through constraint"

**Design Decisions**:

- Single main thread, flat message list
- No swarms or competing agents
- Debuggability and reliability first
- Intentionally low-level and unopinionated

### Architecture

**Agent Loop**:

```python
while(tool_call):
    execute_tool()
    feed_results_back()
    repeat()
```

**Key Characteristics**:

- Classic agent loop (no state machines)
- All tools in single thread
- Subagents as isolated contexts
- MCP for extensibility

### Tool Set

**Core Tools**:
| Tool | Purpose | Notes |
|------|---------|-------|
| Bash | Command execution | With safety guidance |
| Read | File reading | Multimodal (images, PDFs) |
| Write | File creation | Overwrites existing |
| Edit | String replacement | Requires unique match |
| GrepTool | Regex search | Preferred over vector search |
| GlobTool | File pattern matching | Fast discovery |
| WebFetch | URL content extraction | With prompting |
| WebSearch | Web search | Current information |
| Task | Sub-agent spawning | Context isolation |

**Tool Design Philosophy**:

- Each tool has detailed description + instruction prompt
- Guidance on when/how to use
- Examples embedded in tool definitions
- ~10,000 tokens total context

### Sub-Agent System (Task Tool)

**Architecture**: Mixture-of-experts with isolated contexts

**Built-In Agents**:
| Agent | Model | Tools | Purpose |
|-------|-------|-------|---------|
| Explore | Haiku (fast) | Read-only | Codebase search, analysis |
| Plan | Inherits | Read-only | Research for planning mode |
| General-purpose | Inherits | All | Complex multi-step tasks |

**Key Design Decisions**:

1. **Context Isolation**: Each sub-agent has own context window
2. **No Recursion**: Sub-agents cannot spawn other sub-agents
3. **Tool Restrictions**: Can limit tools per agent
4. **Model Selection**: Can use cheaper models (Haiku) for exploration
5. **Background Execution**: Can run concurrently (Ctrl+B)

**Custom Agent Configuration** (`.claude/agents/`):

```markdown
---
description: Code review specialist
tools:
  - Read
  - Grep
  - Glob
disallowedTools:
  - Write
  - Edit
permissionMode: default
model: claude-haiku-4-20250514
---

You are a code reviewer. Analyze code quality, find bugs, suggest improvements.
Never modify files directly - output recommendations only.
```

**Agent Discovery**:

- Built-in agents (Explore, Plan, general-purpose)
- Project-level: `.claude/agents/*.prompt.md`
- User-level: `~/.claude/agents/*.prompt.md`
- Plugin-provided agents

**Invocation Methods**:

- Automatic delegation (Claude decides based on task)
- Explicit: `@agent-name task description`
- CLI flag: `--agent reviewer`
- Command: `/agents` for management UI

**Context Management Benefits**:

- Main conversation stays clean
- Exploration output isolated
- Only relevant summary returned
- Can resume sub-agents later
- Auto-compaction per agent

### Context Management

**Compaction**:

- Automatic summarization when context limit approaches
- Preserves architectural decisions, unresolved bugs, implementation details
- Discards redundant tool outputs
- Continues with compressed context + 5 most recent files

**Strategies**:

1. **Tool Result Clearing**: Remove raw results deep in history
2. **Structured Note-Taking**: Agent writes notes to files, reads back later
3. **Sub-Agent Isolation**: Heavy work in separate context
4. **Progressive Disclosure**: Load data just-in-time

**Quote**: "Claude Code uses this approach to perform complex data analysis over large databases. The model can write targeted queries, store results, and leverage Bash commands like head and tail to analyze large volumes of data without ever loading the full data objects into context."

### Memory Approaches

**Built-In Memory**: None

**Persistence Mechanisms**:
| Method | Description | Scope |
|--------|-------------|-------|
| CLAUDE.md | Project instructions | Loaded at startup |
| Session files | Conversation history | Per-session |
| Sub-agent transcripts | Isolated histories | Separate files |
| Note files | Agent-written notes | Project filesystem |
| Memory tool (beta) | File-based storage | Cross-session |

**Memory Tool** (Beta):

- Store information outside context window
- File-based system for persistence
- Build knowledge bases over time
- Reference previous work

**Opportunity**: No built-in semantic memory, episodic memory, or knowledge graph. This is ion's differentiation opportunity.

### MCP Support (Dual Role)

**As Client**:

- Consumes external MCP servers
- Configuration in `.mcp.json` or settings
- All tools available to agents

**As Server**:

- Exposes tools to other clients
- Claude Desktop, Cursor, Windsurf can invoke
- One-shot agent-in-agent pattern

**Ecosystem**: 90%+ expected adoption (OpenAI, Google, etc. confirmed)

### Skills System

**SKILL.md Format**:

- Markdown files for behavior modification
- Loaded on demand (progressive disclosure)
- ~50 tokens for name/description at startup
- ~1,500+ tokens when fully loaded

**Adoption**: Claude Code, Pi, Codex, Goose, Amp all support SKILL.md

---

## Comparative Analysis

### Tool Design Philosophy

| Aspect            | Pi-Mono                     | Claude Code                  |
| ----------------- | --------------------------- | ---------------------------- |
| **Count**         | 4 tools                     | 9+ tools                     |
| **Guidance**      | Minimal (model knows)       | Extensive (10k tokens)       |
| **Extensibility** | CLI tools + README          | MCP servers                  |
| **Context Cost**  | <1000 tokens                | ~10,000 tokens               |
| **Philosophy**    | "Models are trained enough" | "Guide the model explicitly" |

**Lesson for ion**: Start minimal like Pi, add guidance only when benchmarks show need.

### Sub-Agent Patterns

| Aspect                | Pi-Mono                   | Claude Code                  |
| --------------------- | ------------------------- | ---------------------------- |
| **Implementation**    | None (spawn via bash)     | Task tool + custom agents    |
| **Observability**     | Full (tmux visibility)    | Limited (transcripts only)   |
| **Context Isolation** | Manual (separate session) | Automatic (isolated windows) |
| **Tool Restrictions** | Manual                    | Declarative per-agent        |
| **Recursion**         | Possible (spawn pi again) | Blocked (no nesting)         |

**Lesson for ion**: Consider Pi's "spawn via bash" for simple cases, Claude's Task tool for complex orchestration. Prioritize observability.

### Context Management

| Aspect                  | Pi-Mono               | Claude Code  |
| ----------------------- | --------------------- | ------------ |
| **Compaction**          | Planned (not shipped) | Automatic    |
| **Note-Taking**         | Manual (files)        | Agent-driven |
| **Sub-Agent Isolation** | Manual                | Built-in     |
| **Token Tracking**      | Full                  | Full         |

**Lesson for ion**: Compaction is table stakes. Consider both automatic and manual options.

### Memory/Persistence

| Aspect              | Pi-Mono  | Claude Code        | ion Opportunity       |
| ------------------- | -------- | ------------------ | --------------------- |
| **Episodic**        | None     | None               | OmenDB vectors        |
| **Semantic**        | None     | None               | Hybrid search         |
| **Knowledge Graph** | None     | None               | Entity extraction     |
| **Cross-Session**   | Sessions | Memory tool (beta) | Budget-aware assembly |

**Lesson for ion**: Memory is THE differentiator. Both competitors lack it.

### Design Philosophy Trade-offs

**Pi-Mono's Minimalism**:

- Pros: Fast, low context cost, observable, composable
- Cons: Less capability, manual orchestration, no ecosystem

**Claude Code's Bounded Complexity**:

- Pros: Rich features, ecosystem compatibility, guardrails
- Cons: Higher context cost, some black boxes, vendor lock-in

---

## Recommendations for ion

### Tools: Start Minimal, Add Based on Benchmarks

**Core Tools** (like Pi):

```rust
enum CoreTool {
    Bash,   // Shell execution
    Read,   // File reading
    Write,  // File writing
    Edit,   // String replacement
}
```

**Add Only If Benchmarks Justify**:

- GrepTool (dedicated regex search)
- GlobTool (file patterns)
- WebFetch/WebSearch (research tasks)
- Task (sub-agent orchestration)

### Context Management: Hybrid Approach

**Compaction**: Auto-summarize at 80% capacity (like Claude Code)

**Note-Taking**: Agent-written notes to `ai/tmp/` or memory system

**Sub-Agent Isolation**: Offer both:

1. Simple: Spawn ion via bash (Pi approach)
2. Complex: Task tool with isolated contexts (Claude approach)

### Memory: The Differentiator

**What Neither Has**:

- Budget-aware context assembly
- Semantic search for memories
- ACE scoring for relevance
- Cross-session learning

**ion's Architecture**:

```
User Query
    |
    v
Query Classification (transactional vs. memory-relevant)
    |
    v
Budget-Aware Assembly
    - Token budget
    - Relevance scoring (ACE)
    - RRF fusion for hybrid retrieval
    |
    v
Context Window
```

### MCP: Client First, Server Later

**Phase 1**: MCP client (ecosystem compatibility)
**Phase 2**: MCP server (expose tools to Claude Desktop, etc.)

### Sub-Agents: Observability First

**Principle**: "If you can't see what it does, don't automate it"

**Implementation**:

1. Log all sub-agent decisions
2. Expose transcripts easily
3. Allow manual intervention
4. Consider Pi's "spawn via tmux" for debugging

---

## Key Takeaways

### From Pi-Mono

1. **Four tools are enough** for effective coding agents
2. **Models are already trained** on coding patterns; minimal prompts work
3. **CLI tools beat MCP** for context efficiency (progressive disclosure)
4. **Observability matters** more than convenience
5. **Context is precious** - don't waste on protocol overhead

### From Claude Code

1. **Bounded complexity** can work (with guardrails)
2. **Sub-agent isolation** helps manage context
3. **Compaction is essential** for long sessions
4. **MCP is ecosystem standard** - support it
5. **Skills (SKILL.md)** are the extension mechanism

### For ion

1. **Memory is the differentiator** - neither competitor has it
2. **Start minimal** (4 tools), add based on benchmarks
3. **Budget-aware assembly** is novel and valuable
4. **Observability first** in sub-agent design
5. **Support both philosophies** - minimal mode and full-featured mode

---

## References

**Pi-Mono**:

- [What I learned building a minimal coding agent](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/)
- [GitHub: badlogic/pi-mono](https://github.com/badlogic/pi-mono)
- [Package Architecture](https://deepwiki.com/badlogic/pi-mono/1.1-package-architecture)
- [Official Site](https://shittycodingagent.ai/)

**Claude Code**:

- [Sub-Agents Documentation](https://docs.claude.com/en/docs/claude-code/subagents)
- [Best Practices](https://www.anthropic.com/engineering/claude-code-best-practices)
- [Context Engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
- [Building Agents with Claude Agent SDK](https://www.anthropic.com/engineering/building-agents-with-the-claude-agent-sdk)
- [Sub-Agents Deep Dive](https://deepwiki.com/zebbern/claude-code-guide/6-sub-agents-system)
- [Context Management with Subagents](https://www.richsnapp.com/article/2025/10-05-context-management-with-subagents-in-claude-code)

**Benchmarks**:

- [Terminal-Bench](https://github.com/laude-institute/terminal-bench)
- Terminus 2 (minimal agent holding its own with sophisticated tooling)

---

## Change Log

- 2026-01-16: Initial research compiled from multiple sources
