# TUI Coding Agents Comparison 2026

**Research Date**: 2026-01-12
**Purpose**: Compare leading terminal-based AI coding agents
**Scope**: Features, architecture, extensibility, MCP/ACP support

---

## Executive Summary

| Agent           | Language    | Stars | Providers      | MCP/ACP                | Extension Model       | License     |
| --------------- | ----------- | ----- | -------------- | ---------------------- | --------------------- | ----------- |
| **Claude Code** | TypeScript  | N/A   | Claude only    | MCP (client + server)  | Skills (SKILL.md)     | Proprietary |
| **Gemini CLI**  | TypeScript  | 90.6k | Gemini/Vertex  | MCP (client)           | Extensions            | Apache-2.0  |
| **Codex CLI**   | Rust        | 56k   | OpenAI/ChatGPT | MCP (client)           | AGENTS.md + Tasks     | Apache-2.0  |
| **OpenCode**    | Go + TS/Bun | 50k+  | 75+ via AI SDK | MCP (client)           | Custom agents         | MIT         |
| **Amp**         | TypeScript  | N/A   | Multi-model    | MCP (client)           | Skills + Threads      | Proprietary |
| **Droid**       | Go          | N/A   | Multi-model    | MCP (via droid-mode)   | AGENTS.md + Subagents | Proprietary |
| **Toad**        | Python      | 1.6k  | Via ACP hosts  | ACP (host)             | ACP protocol          | AGPL-3.0    |
| **Pi**          | TypeScript  | N/A   | 15+ direct     | **No MCP** (by design) | Extensions + Skills   | MIT         |

---

## Agent Deep Dives

### 1. Claude Code (Anthropic)

**Architecture**: Classic agent loop with simplicity through constraint

**Core Features**:

- Single main thread, flat message list
- No swarms or competing agents
- GrepTool (regex) instead of vector search
- Subagents with limited tool sets (no spawning)
- 84% reduction in permission prompts via sandboxing

**Tools**:

- `Bash` - Command execution
- `Read` - File reading
- `Write` - File writing
- `Edit` - String replacement
- `GrepTool` - Regex search
- `GlobTool` - File pattern matching

**Plugin System**: Skills (SKILL.md format)

- Progressive disclosure: names/descriptions at startup (~50 tokens)
- Full load on demand (~1,500+ tokens)
- Claude-specific but adopted by Goose, Pi, Codex

**MCP Support**: Dual role (client AND server)

- As client: consumes external MCP servers
- As server: exposes tools to Claude Desktop, Cursor, etc.

**Memory**: None (opportunity for differentiation)

**What Makes It Good**:

- Simplicity and debuggability
- Checkpoints for state save/restore
- Context compaction (compact feature)
- Deep model integration (Claude 4.5)

**License**: Proprietary (Anthropic)

---

### 2. Gemini CLI (Google)

**Architecture**: TypeScript/Node with Google AI integration

**Core Features**:

- 1M token context window (Gemini 2.5 Pro)
- Google Search grounding built-in
- Sandboxing for safe execution
- Trusted folders for execution policies
- Conversation checkpointing

**Tools**:

- File system operations
- Shell commands
- Web fetch with Google Search grounding
- MCP integration

**Plugin System**: Extensions

- GEMINI.md for project-specific context
- MCP servers for custom integrations

**MCP Support**: Full client support

```json
{
  "mcpServers": {
    "my-server": {
      "command": "node",
      "args": ["server.js"]
    }
  }
}
```

**Authentication Options**:

- Google OAuth (60 req/min, 1000/day free)
- API Key (100 req/day free)
- Vertex AI (enterprise)

**Memory**: None

**What's Unique**:

- Free tier with generous limits
- Native Google Search grounding
- 1M token context window
- Enterprise-ready (Vertex AI integration)

**License**: Apache-2.0

---

### 3. Codex CLI (OpenAI)

**Architecture**: Rust-based task loop with orchestrator pattern

**Core Features**:

- Multi-turn completion loop (continues until model stops)
- Auto-compaction at token limit
- 57.8% Terminal-Bench accuracy
- Task-based workflow architecture
- Parallel tool execution (FuturesOrdered)

**Tools**:

- Three-stage dispatch: Build ToolCall -> ToolRegistry -> Failure handling
- ToolOrchestrator: Approval -> Execution -> Retry

**Task Types**:

- `RegularTask` - Standard chat/coding
- `ReviewTask` - Code review sub-agent
- `CompactTask` - Conversation summarization
- `UndoTask` - Revert changes
- `GhostSnapshotTask` - Parallel commit tracking

**Plugin System**: AGENTS.md + Task-based

- Model-specific prompts (GPT-5, GPT-5 Codex, O1)
- Per-project instructions via .codexrc
- Structured review with priority levels (P0-P3)

**MCP Support**: Client support for external tools

**Memory**: None (session history only)

**What's Unique**:

- Rust core for performance
- Approval caching with escalation on failure
- Structured review sub-agent
- ChatGPT Plus/Pro account integration

**License**: Apache-2.0

---

### 4. OpenCode

**Architecture**: Client/Server with Go TUI + Bun/Hono backend

**Core Features**:

- 75+ LLM providers via AI SDK + Models.dev
- LSP integration (automatically loads appropriate LSPs)
- Multi-session parallel execution
- Shareable session links
- Auto-compact at 95% context window
- Claude Pro/ChatGPT Plus account login

**Tools**:

```typescript
const BUILTIN = [
  BashTool,
  EditTool,
  WebFetchTool,
  GlobTool,
  GrepTool,
  ListTool,
  ReadTool,
  WriteTool,
  TodoWriteTool,
  TodoReadTool,
  TaskTool,
];
```

**Plugin System**: Custom agents + MCP

- Per-agent tools, prompts, model
- Plan agent: No edit tool, restricted bash
- Build agent: Full tool access

**MCP Support**: Full client support

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem"]
    }
  }
}
```

**Memory**: None

**What's Unique**:

- 75+ provider support (most flexible)
- Multi-session parallel execution
- LSP-enhanced context
- Provider-agnostic architecture
- Desktop app + IDE extension + CLI

**License**: MIT

---

### 5. Amp (Sourcegraph)

**Architecture**: TypeScript with enterprise focus

**Core Features**:

- Frontier model optimization
- Thread-based workflows
- Agentic code review
- Sourcegraph code search integration
- Pay-as-you-go pricing (no markup)
- $10/day free ad-supported tier

**Tools**:

- File operations
- Shell commands
- Code search (Sourcegraph)
- MCP integration

**Plugin System**: Skills + Shared Threads

- Agent Skills (on-demand loading)
- Threads shared by default for team reuse
- Subagents (Oracle, Librarian, etc.)

**MCP Support**: Full client support

- Lazy loading MCP tools (context-efficient)

**Memory**: None (thread history)

**What's Unique**:

- Team collaboration via shared threads
- Sourcegraph code intelligence
- Agentic review for agent-generated code
- Multi-agent panel (view multiple threads)
- User-invokable skills

**License**: Proprietary (Sourcegraph)

---

### 6. Droid (Factory)

**Architecture**: Spec-first autonomous execution

**Core Features**:

- 58% Terminal-Bench accuracy (highest)
- Spec-first planning (YAML)
- Autonomous self-correction
- Multi-environment: CLI, IDE, Web, Slack, Linear

**Execution Pattern**:

```
User Task
    |
1. SPEC GENERATION (YAML)
   - Steps to execute
   - Expected outputs
   - Validation criteria
    |
2. SPEC VALIDATION
    |
3. AUTONOMOUS EXECUTION LOOP
   Execute -> Test -> Self-correct -> Retry
    |
4. FINAL VERIFICATION
```

**Plugin System**: AGENTS.md + Custom Droids (Subagents)

- Engineering Droid
- Reliability Droid
- Product Droid
- Knowledge Droid
- Custom Slash Commands

**MCP Support**: Yes, via droid-mode

**Memory**: None

**What's Unique**:

- Spec-first planning before execution
- Highest Terminal-Bench accuracy
- Multi-environment deployment
- Integrations: GitHub, Linear, Slack
- Custom subagent ("Droid") definitions

**License**: Proprietary (Factory)

---

### 7. Toad (Will McGugan)

**Architecture**: Python with Textual TUI framework

**Core Features**:

- Unified interface for multiple AI agents
- ACP (Agent Client Protocol) host
- Rich Markdown rendering
- Fuzzy file search with gitignore filtering
- Web server mode
- Built by Textual creator

**Supported Agents** (via ACP):

- OpenHands
- Claude Code
- Gemini CLI
- Codex CLI
- And more...

**Plugin System**: ACP protocol

- Any ACP-compatible agent can run under Toad
- No custom extension system

**MCP Support**: No (ACP instead)

- ACP is for agent UI, not tool integration

**Memory**: None (deferred to underlying agents)

**What's Unique**:

- Universal frontend for any ACP-compatible agent
- Beautiful TUI (Textual framework)
- Agent-agnostic interface
- Web server mode for browser access
- Will McGugan's TUI expertise

**License**: AGPL-3.0

---

### 8. Pi (badlogic / Mario Zechner)

**Architecture**: TypeScript with custom differential TUI

**Philosophy**: "Minimal. Opinionated. Extensible."

**Core Features**:

- 4 core tools: read, write, edit, bash
- YOLO mode by default (no permission popups)
- Session tree with branching
- Auto-compaction for long conversations
- Model cycling (Ctrl+P)
- Custom differential TUI (pi-tui)

**Providers**:

- Anthropic, OpenAI, Google, Mistral, Groq
- Cerebras, xAI, OpenRouter, ZAI, MiniMax
- Amazon Bedrock, Ollama
- OAuth for Copilot, Gemini CLI, Claude, Codex

**What Pi Doesn't Build (by design)**:

- **No MCP** - "Build CLI tools with READMEs instead"
- **No sub-agents** - "Spawn pi instances via tmux"
- **No permission popups** - "Security theater"
- **No plan mode** - "Write plans to file, start fresh"
- **No background bash** - Explicit control

**Plugin System**: Extensions + Skills

- AGENTS.md support
- SKILL.md (Claude Code compatible)
- Prompt templates (/commands)
- Custom extensions (tools, hooks, UI)
- SDK for embedding

**MCP Support**: **None** (intentional)

> "Popular MCP servers dump 7-9% of your context window before you start. Skills load on-demand instead."

**Memory**: None

**What's Unique**:

- Minimal context footprint
- Differential TUI rendering (efficient)
- Deliberate anti-patterns (no MCP, no subagents)
- Skills as alternative to MCP
- Clean provider abstraction

**License**: MIT

---

## Feature Classification

### Table Stakes (Must Have)

| Feature                   | Notes                           |
| ------------------------- | ------------------------------- |
| **File operations**       | read, write, edit, glob, grep   |
| **Shell execution**       | bash/shell command running      |
| **Multi-provider**        | At minimum 3-4 major providers  |
| **Session persistence**   | Save/resume conversations       |
| **Context management**    | Auto-compaction, token tracking |
| **AGENTS.md / CLAUDE.md** | Project-specific instructions   |
| **Skills/Prompts**        | SKILL.md or equivalent          |

### Nice-to-Have (Competitive)

| Feature             | Agents With It          | Notes                       |
| ------------------- | ----------------------- | --------------------------- |
| **MCP Client**      | All except Pi           | Standard protocol adoption  |
| **Git integration** | Most                    | Commits, checkpoints, undo  |
| **Sandboxing**      | Claude Code, Gemini CLI | 84% reduction in prompts    |
| **LSP integration** | OpenCode                | Semantic code understanding |
| **Multi-session**   | OpenCode                | Parallel execution          |
| **Subagents**       | Claude Code, Amp, Droid | Specialized task delegation |
| **Image support**   | Most                    | Multimodal input            |

### Innovative Differentiators

| Feature              | Agent       | Innovation                    |
| -------------------- | ----------- | ----------------------------- |
| **ACP Host**         | Toad        | Universal agent frontend      |
| **No MCP**           | Pi          | Context efficiency via Skills |
| **Spec-first**       | Droid       | Planning before execution     |
| **Shared Threads**   | Amp         | Team collaboration            |
| **MCP Server**       | Claude Code | Dual client/server role       |
| **75+ Providers**    | OpenCode    | Maximum flexibility           |
| **Differential TUI** | Pi          | Efficient terminal rendering  |
| **Google Search**    | Gemini CLI  | Built-in grounding            |

---

## Comparison Matrix

| Feature             | Claude | Gemini | Codex       | OpenCode  | Amp           | Droid  | Toad       | Pi        |
| ------------------- | ------ | ------ | ----------- | --------- | ------------- | ------ | ---------- | --------- |
| **Core**            |
| Language            | TS     | TS     | Rust        | Go+TS     | TS            | Go     | Python     | TS        |
| Open Source         | No     | Yes    | Yes         | Yes       | No            | No     | Yes        | Yes       |
| License             | Prop   | Apache | Apache      | MIT       | Prop          | Prop   | AGPL       | MIT       |
| **Providers**       |
| Multi-provider      | No     | No     | No          | Yes (75+) | Yes           | Yes    | Via host   | Yes (15+) |
| Local models        | No     | No     | No          | Yes       | Yes           | Yes    | Via host   | Yes       |
| Free tier           | No     | Yes    | Via ChatGPT | Yes       | Yes ($10/day) | No     | N/A        | Yes       |
| **Tools**           |
| File ops            | Yes    | Yes    | Yes         | Yes       | Yes           | Yes    | Via host   | Yes       |
| Shell               | Yes    | Yes    | Yes         | Yes       | Yes           | Yes    | Via host   | Yes       |
| LSP                 | No     | No     | No          | Yes       | No            | No     | Via host   | No        |
| Web fetch           | No     | Yes    | Yes         | Yes       | Yes           | Yes    | Via host   | No        |
| **Extension**       |
| MCP Client          | Yes    | Yes    | Yes         | Yes       | Yes           | Yes    | No         | No        |
| MCP Server          | Yes    | No     | No          | No        | No            | No     | No         | No        |
| ACP                 | No     | No     | No          | No        | No            | No     | Yes (host) | No        |
| Skills              | Yes    | No     | Yes         | No        | Yes           | Yes    | Via host   | Yes       |
| Extensions          | Skills | Ext    | Tasks       | Agents    | Skills        | Droids | ACP        | Ext       |
| **Features**        |
| Sandboxing          | Yes    | Yes    | Yes         | No        | No            | No     | Via host   | No        |
| Subagents           | Yes    | No     | Yes         | Yes       | Yes           | Yes    | No         | No        |
| Git integration     | Yes    | No     | Yes         | Yes       | Yes           | Yes    | Via host   | Yes       |
| Session persistence | Yes    | Yes    | Yes         | Yes       | Yes           | Yes    | Planned    | Yes       |
| **Memory**          |
| Episodic            | No     | No     | No          | No        | No            | No     | No         | No        |
| Semantic            | No     | No     | No          | No        | No            | No     | No         | No        |
| Knowledge graph     | No     | No     | No          | No        | No            | No     | No         | No        |

---

## Architectural Patterns

### Provider Abstraction

| Pattern             | Agents      | Approach                         |
| ------------------- | ----------- | -------------------------------- |
| **SDK-based**       | OpenCode    | Vercel AI SDK for unified access |
| **Custom traits**   | Pi, Codex   | Provider trait + adapters        |
| **Single provider** | Claude Code | No abstraction needed            |
| **Factory pattern** | Goose       | Runtime provider dispatch        |

### Tool System

| Pattern               | Agents          | Approach                   |
| --------------------- | --------------- | -------------------------- |
| **Registry + Router** | OpenCode, Codex | Centralized dispatch       |
| **Direct execution**  | Claude Code, Pi | Simple tool calls          |
| **Orchestrator**      | Codex           | Approval + Sandbox + Retry |
| **MCP-first**         | Gemini CLI      | All tools via MCP          |

### Extension Model

| Model                 | Agents                 | Approach                |
| --------------------- | ---------------------- | ----------------------- |
| **Skills (SKILL.md)** | Claude, Pi, Codex, Amp | Prompt injection        |
| **MCP servers**       | Most                   | External tool providers |
| **Custom agents**     | OpenCode, Droid        | Per-task agent configs  |
| **Extensions**        | Pi, Gemini             | Code-based plugins      |

---

## Recommendations for Aircher

### Features to Adopt

| Feature                 | Source          | Rationale                          |
| ----------------------- | --------------- | ---------------------------------- |
| **Skills (SKILL.md)**   | Claude Code, Pi | Standard format, context-efficient |
| **Multi-provider**      | OpenCode        | Flexibility, no vendor lock-in     |
| **MCP Client**          | Most            | Ecosystem compatibility            |
| **Session persistence** | All             | User expectation                   |
| **AGENTS.md**           | All             | Project customization              |
| **Auto-compaction**     | Most            | Long conversation support          |

### Differentiation Opportunities

| Opportunity          | Current Gap            | Aircher Approach                  |
| -------------------- | ---------------------- | --------------------------------- |
| **Memory**           | No agent has it        | OmenDB integration (budget-aware) |
| **Semantic search**  | Claude uses regex only | Hybrid search with vectors        |
| **Context assembly** | Basic compaction       | ACE scoring, RRF fusion           |
| **Knowledge graph**  | None                   | Entity extraction, relationships  |

### Architecture Decision

**Recommendation**: Full Rust with OmenDB Memory

| Component | Choice        | Why                             |
| --------- | ------------- | ------------------------------- |
| Core      | Rust          | Single binary, performance      |
| TUI       | ratatui       | Mature, async-friendly          |
| Provider  | Trait-based   | Flexibility, multiple providers |
| Memory    | Native OmenDB | No IPC overhead                 |
| MCP       | Client first  | Ecosystem compatibility         |
| Skills    | SKILL.md      | Standard format                 |

---

## References

**Claude Code**:

- https://www.anthropic.com/engineering/claude-code-sandboxing
- https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills
- https://docs.anthropic.com/en/docs/claude-code/mcp

**Gemini CLI**:

- https://github.com/google-gemini/gemini-cli
- https://geminicli.com/docs/

**Codex CLI**:

- https://github.com/openai/codex
- https://developers.openai.com/codex/cli/

**OpenCode**:

- https://github.com/opencode-ai/opencode
- https://opencode.ai/

**Amp**:

- https://ampcode.com/
- https://sourcegraph.com/amp

**Droid (Factory)**:

- https://factory.ai/
- https://docs.factory.ai/cli/configuration/agents-md

**Toad**:

- https://github.com/batrachianai/toad
- https://willmcgugan.github.io/toad-released/

**Pi**:

- https://github.com/badlogic/pi-mono
- https://mariozechner.at/posts/2025-11-30-pi-coding-agent/
- https://shittycodingagent.ai/

---

## Key Takeaways

1. **MCP is table stakes** - All major agents support it (except Pi, which is intentionally minimal)

2. **Skills are the new standard** - SKILL.md format adopted across Claude, Pi, Codex, Amp

3. **Memory is unexplored** - No agent has persistent memory beyond sessions (opportunity)

4. **Spec-first wins benchmarks** - Droid's approach achieves 58% on Terminal-Bench

5. **Simplicity works** - Claude Code's flat loop outperforms complex architectures

6. **Provider flexibility matters** - OpenCode's 75+ providers shows demand

7. **ACP is emerging** - Toad proves value of universal agent interfaces

8. **Pi's minimalism is valid** - Context efficiency over feature bloat
