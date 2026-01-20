# TUI/CLI AI Coding Agent Architecture Analysis

**Research Date**: 2026-01-12
**Updated**: 2026-01-12 (added Goose, Pi, Letta deep dives)
**Purpose**: Architectural patterns for building aircher TUI agent
**Focus**: Provider abstraction, tool systems, plugins, MCP, memory

---

## Executive Summary

| Agent           | Language        | Provider Abstraction | Tool System          | Plugin/Extension   | MCP Support               | Memory        |
| --------------- | --------------- | -------------------- | -------------------- | ------------------ | ------------------------- | ------------- |
| **Goose**       | Rust + Electron | 20+ providers        | MCP extensions       | Skills + MCP       | Full (client, SDK author) | Tag-based MCP |
| **OpenCode**    | Go + TS/Bun     | AI SDK (unified)     | Registry + Router    | Custom agents, MCP | Full (client)             | None          |
| **Claude Code** | TypeScript      | Claude-only          | Meta-tool Skills     | Skills (SKILL.md)  | Full (client + server)    | None          |
| **Pi**          | TypeScript      | Custom unified API   | Built-in only        | Skills only        | **No** (by design)        | None          |
| **Codex CLI**   | Rust            | OpenAI-focused       | Orchestrator pattern | Task-based         | Yes (client)              | None          |
| **Gemini CLI**  | TypeScript      | Gemini-focused       | Built-in + MCP       | Extensions system  | Full (client)             | None          |
| **Letta**       | Node.js         | Platform-based       | MCP tools            | Subagents          | Yes                       | **Stateful**  |
| **Aider**       | Python          | LiteLLM              | Coder classes        | Edit formats       | No                        | None          |
| **Crush CLI**   | Go              | Multi-provider       | LSP-enhanced         | MCP servers        | Full (client)             | None          |

**Key Insights**:

1. **Goose is the reference** - Rust core, MCP ecosystem leader, contributed to Linux Foundation AAIF
2. **MCP** is table stakes - all modern agents except Pi support it
3. **Skills** are standard - SKILL.md format (Claude Code) adopted by Goose, Pi, Codex CLI
4. **Memory is underexplored** - only Letta and Goose have it, both via MCP (opportunity for OmenDB)
5. **Pi's minimalism** - no MCP, no subagents, but lightweight and proven TypeScript TUI

---

## 0. Goose (Reference Implementation)

**Architecture**: Rust workspace + Electron desktop + Go scheduler

### Project Structure

```
crates/
├── goose           # Core: agents, providers, context_mgmt, session
├── goose-cli       # CLI entry point
├── goose-server    # Backend binary (goosed)
├── goose-mcp       # MCP extensions
├── mcp-client      # Became official Rust MCP SDK
├── mcp-core        # Shared MCP types
├── mcp-server      # MCP server impl
temporal-service/   # Go scheduler
ui/desktop/         # Electron desktop app
```

### Provider Abstraction

**20+ providers** supported:

- Cloud: Anthropic, OpenAI, Google, Azure, AWS Bedrock, GCP Vertex AI
- Platforms: GitHub Copilot, Databricks, Snowflake, LiteLLM, OpenRouter, xAI
- Local: Ollama

Factory pattern for provider creation:

```rust
pub use factory::{create, create_with_default_model, create_with_named_model, providers};
```

### Tool System (MCP-First)

**All tools are MCP extensions**:

- Built-in extensions: developer, web scraping, automation, memory
- External MCP servers via stdio/SSE/HTTP
- 3,000+ MCP servers available in ecosystem

**Extension types**:

- Platform extensions (always available): Chat Recall, Code Execution, Skills, Todo
- User extensions (configurable via config.yaml)

### Skills System

**SKILL.md format** (Claude Code compatible):

- Discovery: `~/.config/goose/skills/` or `~/.claude/skills/`
- Progressive disclosure: metadata at startup (~50 tokens), full load on demand (~2-5K tokens)
- Project-level: `./.goose/skills/` or `./.agents/skills/`

### Memory Extension

**Tag-based MCP memory**:

- Trigger words: "remember", "forget", "search memory", "save"
- Loads all memories at session start
- Local or global scoping
- Keyword tagging for organization

**Key limitation**: Full context injection (no selective loading based on relevance)

### Sessions

- JSONL persistence with auto-backup
- Desktop: persistent memory across sessions
- CLI: fresh start each session

### Key Differentiators

- **25k+ stars**, 350+ contributors
- **Official Rust MCP SDK** originated from Goose
- **Linux Foundation AAIF** contribution (alongside Anthropic MCP, OpenAI AGENTS.md)
- **Dual interfaces**: CLI + Electron desktop
- **Context revision**: smaller LLM summarization, algorithmic deletion

### Key Crates Used

```toml
tokio = "1.43"        # Async runtime
reqwest = "0.12"      # HTTP client
axum = "0.8"          # Web framework
sqlx = "0.8"          # SQLite (sessions)
tiktoken-rs = "0.6"   # Token counting
minijinja = "2.12"    # Prompt templating
tracing = "*"         # Observability
```

---

## 0.5 Pi (Lightweight Reference)

**Architecture**: TypeScript with custom TUI (pi-tui), minimal by design

### Philosophy

> "Popular MCP servers dump 7-9% of your context window before you start. Skills load on-demand instead."

**Deliberate constraints**:

- **No MCP** - Skills instead
- **No subagents** - spawn separate instances via tmux
- **Unified LLM API** - pi-ai package

### Provider Abstraction

Custom unified API supporting:

- OpenRouter (recommended)
- OpenAI
- Anthropic
- Google
- Ollama (local)

Config: `~/.pi/agent/settings.json` + `models.json`

### Tool System

**Built-in only** (no extensibility):

- File operations
- Shell execution
- Git integration
- Search

### Skills System

**SKILL.md format** (Claude Code compatible):

- Location: `~/.pi/skills/`
- On-demand loading
- No MCP overhead

### Key Differentiators

- **Minimal footprint** - reaction to Claude Code bloat
- **Custom TUI** (pi-tui) - not ink or blessed
- **Differential rendering** - efficient terminal updates
- **Fast iteration** - small codebase, quick changes

### Source

- Blog: https://mariozechner.at/posts/2025-11-30-pi-coding-agent/
- GitHub: https://github.com/badlogic/pi-mono

---

## 1. OpenCode

**Architecture**: Client/Server with Go TUI + Bun/Hono backend

### Provider Abstraction

```
User → Go TUI → HTTP/SSE → Bun/Hono Server → AI SDK → LLM Providers
```

**Approach**: Uses Vercel AI SDK for provider-agnostic LLM access

- **Single interface** for OpenAI, Anthropic, Gemini, Azure, Bedrock, etc.
- **OpenAI-compatible** endpoint support for self-hosted models
- **Provider-specific system prompts** (gemini.txt, anthropic.txt, etc.)

**Key Files**:

- `packages/opencode/src/session/` - Session and prompt management
- `internal/llm/` - Go-side provider configuration
- `internal/config/` - Model/provider settings

### Tool System

**Pattern**: Registry + Router with plugin hooks

```typescript
const BUILTIN = [
  BashTool, // Execute shell commands
  EditTool, // Edit files
  WebFetchTool, // Fetch URLs
  GlobTool, // Find files by pattern
  GrepTool, // Search file contents
  ListTool, // List directories
  ReadTool, // Read files
  WriteTool, // Write files
  TodoWriteTool, // Todo list management
  TodoReadTool, // Read todos
  TaskTool, // Launch sub-agents
];
```

**Tool Dispatch Flow**:

1. `ToolRegistry.tools()` - Get available tools for model
2. `Wildcard.all()` - Filter by enabled patterns
3. `tool.execute()` - Run with session context
4. Plugin hooks: pre/post tool execution

**For MCP Tools**:

```typescript
for (const [key, item] of Object.entries(await MCP.tools())) {
  tools[key] = item; // MCP tools added to same registry
}
```

### Plugin/Extension

**Sub-Agents**: Each agent has own tools, prompts, model

- `plan` agent: No edit tool, restricted bash
- `build` agent: Full tool access
- Custom agents via config file

**MCP Integration**:

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

### Key Differentiators

- **75+ providers** via AI SDK + OpenRouter
- **Multi-session** parallel execution
- **LSP integration** for diagnostics
- **Shareable sessions** for collaboration
- **Auto-compact** at 95% context window

---

## 2. Claude Code

**Architecture**: Simple agent loop with meta-tool Skills

### Provider Abstraction

**Single provider** (Anthropic Claude) - no abstraction layer needed

```python
# Conceptual core loop
while(tool_call):
    execute_tool()
    feed_results_back()
    repeat()
```

**Design Philosophy**: "Simplicity through constraint"

- Single main thread, flat message list
- No swarms, no competing agents
- Intentionally low-level, close to raw model

### Tool System

**Built-in Tools**:

- `Bash` - Command execution
- `Read` - File reading
- `Write` - File writing
- `Edit` - String replacement
- `GrepTool` - Regex search (no vector DB!)
- `GlobTool` - File pattern matching

**Subagents**: Full agents with limited tool sets

- Cannot spawn other subagents (prevents infinite nesting)
- Inherit tools from main thread or get restricted set
- Per-subagent model selection possible

### Skills System (Plugin Architecture)

**Key Innovation**: Prompt expansion via markdown files

```markdown
# SKILL.md

---

name: pdf-reader
description: Extract text from PDF files
allowed-tools:

- Bash(pdftotext:\*)
- Read
- Write
  model: claude-sonnet-4

---

## Instructions

When processing PDF files:

1. Use pdftotext to extract content
2. Read the output file
3. Present to user
```

**How Skills Work**:

1. **Progressive Disclosure**: Only skill names/descriptions loaded initially
2. **Selection**: LLM reasoning matches intent to skill
3. **Execution**: Full prompt injected as `isMeta: true` user message
4. **Context Modification**: Tool permissions + model override applied

**Dual-Channel Communication**:

- `isMeta: false` - Visible to user (status indicator)
- `isMeta: true` - Hidden from UI, sent to API (full instructions)

**Skills vs Tools**:
| Feature | Normal Tool | Skill |
|---------|-------------|-------|
| Essence | Direct action | Prompt injection |
| Persistence | Tool call only | Turn + skill context |
| Token Overhead | ~100 tokens | ~1,500+ tokens |
| Use Case | Simple tasks | Complex workflows |

### MCP Support

**Dual Role**: Both client AND server

- **As Client**: Consumes external MCP servers
- **As Server**: Exposes tools to Claude Desktop, Cursor, etc.

### Key Differentiators

- **84% reduction** in permission prompts via sandboxing
- **Checkpoints** for state save/restore
- **Skills** for customization without code
- **Compact feature** for context management

---

## 3. OpenAI Codex CLI

**Architecture**: Rust-based task loop with orchestrator pattern

### Provider Abstraction

**OpenAI-focused** with model family support

```rust
// Model-specific prompts
match model_family {
    GPT5Codex => load_prompt("gpt_5_codex_prompt.md"),
    GPT5 => load_prompt("gpt_5_prompt.md"),
    O1 => load_prompt("o1_prompt.md"),
}
```

### Tool System

**Three-Stage Dispatch**:

```rust
ToolRouter::dispatch_tool_call()
  ↓
1. Build ToolCall from ResponseItem
   - Function call vs Custom tool vs MCP tool
   ↓
2. ToolRegistry::dispatch(ToolInvocation)
   - Routes to appropriate handler
   ↓
3. Failure handling
   - Fatal errors → propagate
   - Recoverable → feedback to model
```

**ToolOrchestrator Pattern**:

```
1. APPROVAL PHASE
   - Check if tool needs approval
   - Get sandbox risk assessment
   - Cache approval for session

2. EXECUTION PHASE
   - Select sandbox (None/Restricted/Full)
   - Execute tool
   - On sandbox denial → escalate

3. RETRY DECISION
   - Should tool retry on failure?
   - Config-driven decisions
```

**Parallel Execution**: `FuturesOrdered` for concurrent tool calls

### Task Types (Plugin Pattern)

```rust
pub trait SessionTask {
    fn kind(&self) -> TaskKind;
    async fn run(...) -> Option<String>;
    async fn abort(...);
}
```

**Built-in Tasks**:

- `RegularTask` - Standard chat/coding
- `ReviewTask` - Code review sub-agent
- `CompactTask` - Conversation summarization
- `UndoTask` - Revert changes
- `GhostSnapshotTask` - Parallel commit tracking

### Key Differentiators

- **Multi-turn completion loop** (continues until model stops)
- **Auto-compaction** at token limit
- **Structured review** with priority levels (P0-P3)
- **Approval caching** with escalation on failure
- **57.8% Terminal-Bench accuracy**

---

## 4. Gemini CLI

**Architecture**: TypeScript/Node with Google AI integration

### Provider Abstraction

**Gemini-focused** with multiple auth options

- **Personal Google Account**: 60 req/min, 1000/day free
- **API Key**: 100 req/day free tier
- **Vertex AI**: Enterprise with billing

### Tool System

**Built-in Tools**:

- File system operations
- Shell commands
- Web fetch with Google Search grounding
- Custom extensions

**MCP Integration**:

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

### Extension System

- Custom commands via extensions
- GEMINI.md for project-specific context
- Conversation checkpointing

### Key Differentiators

- **1M token context window** (Gemini 2.5 Pro)
- **Google Search grounding** built-in
- **Open source** (Apache 2.0)
- **Sandboxing** for safe execution
- **Trusted folders** for execution policies

---

## 5. Aider

**Architecture**: Python CLI with git-native workflow

### Provider Abstraction

**LiteLLM** for unified provider access

```python
# Supports 15+ providers
model = litellm.completion(
    model="claude-3-sonnet",  # or gpt-4, gemini-pro, etc.
    messages=messages
)
```

### Tool/Coder System

**Coder Classes** with edit formats:

```python
class BaseCoder:
    # Core editing logic

class EditBlockCoder(BaseCoder):
    # SEARCH/REPLACE format

class UnifiedDiffCoder(BaseCoder):
    # Unified diff format

class WholeFileCoder(BaseCoder):
    # Full file replacement
```

**Edit Formats**:

- `diff` - Unified diff (most reliable)
- `whole` - Full file replacement
- `search-replace` - Block-based edits
- Model-specific defaults

### Repo Map (Context)

```python
class RepoMap:
    def get_repo_map(self):
        # Use tree-sitter for code structure
        # Build dependency graph
        # Generate concise map for context
```

### Key Differentiators

- **Voice-to-code** input
- **Watch mode** for IDE integration
- **Git-native** with automatic commits
- **Repo map** for intelligent context
- **Prompt caching** for cost savings
- **No MCP support** currently

---

## 6. Amp (Sourcegraph)

**Architecture**: TypeScript with enterprise focus

### Provider Abstraction

Multi-provider with Sourcegraph code intelligence

### Tool System

- Sourcegraph code search integration
- Code review automation
- Thread-based workflows

### Key Differentiators

- **Team collaboration** via shared threads
- **Sourcegraph integration** for code search
- **Enterprise-grade** security
- **Pay-as-you-go** pricing

---

## 7. Factory Droid

**Architecture**: Spec-first autonomous execution

### Execution Pattern

```
User Task
    ↓
1. SPEC GENERATION (YAML)
   - Steps to execute
   - Expected outputs
   - Validation criteria
    ↓
2. SPEC VALIDATION
   - Verify completeness
    ↓
3. AUTONOMOUS EXECUTION LOOP
   Execute → Test → Self-correct → Retry
    ↓
4. FINAL VERIFICATION
```

### Key Differentiators

- **58% Terminal-Bench accuracy** (highest)
- **Spec-first** planning
- **Autonomous self-correction**
- **AGENTS.md** configuration
- **MCP support** via droid-mode

---

## 8. Crush CLI

**Architecture**: Go with Charm TUI framework

### Provider Abstraction

Multi-provider with OpenAI-compatible API support

### Tool System

- **LSP-enhanced** for semantic code understanding
- **First-class MCP** (http/stdio/sse)
- **Permission system** with "yolo" mode

### Key Differentiators

- **Built on Bubble Tea** (beautiful TUI)
- **LSP integration** native
- **Cross-platform** (macOS, Linux, Windows)
- **Successor to OpenCode** (same creator)

---

## Architectural Recommendations for Aircher

### Architecture Decision

| Option               | Approach                      | Pros                           | Cons                                     | Effort |
| -------------------- | ----------------------------- | ------------------------------ | ---------------------------------------- | ------ |
| **A: Full Rust**     | Rust TUI + Rust OmenDB Memory | Single binary, no IPC, fastest | Most work, port memory logic             | High   |
| **B: Rust + MCP**    | Rust TUI + TS OmenDB MCP      | Use existing MCP, less work    | IPC overhead, spawned process            | Medium |
| **C: Bun/TS**        | TS TUI + OmenDB SDK           | Direct SDK, fastest iteration  | Larger binary, more runtime              | Low    |
| **D: Goose contrib** | Contribute to Goose           | Leverage 25k stars, proven     | Less control, must follow their patterns | Medium |

**Recommended**: Option B (Rust + MCP) or hybrid (Rust core + TS MCP servers)

**Rationale**:

1. Goose proves Rust core + MCP extensions works at scale
2. OmenDB MCP already exists - can use immediately
3. Can port memory to native Rust later if performance matters
4. Skills, subagents, providers can all be Rust-native

### Learning from Goose

| Component | Goose Approach               | Aircher Approach                |
| --------- | ---------------------------- | ------------------------------- |
| Core      | Rust workspace               | Rust workspace                  |
| TUI       | Electron (heavy)             | ratatui (lightweight)           |
| MCP       | Full (client + server + SDK) | Client first                    |
| Memory    | Tag-based, full inject       | OmenDB: budget-aware, selective |
| Skills    | SKILL.md (Claude-compat)     | SKILL.md (Claude-compat)        |
| Providers | 20+ factory pattern          | OpenRouter primary + direct     |

**Key differentiation**: OmenDB Memory with budget-aware context assembly (vs Goose's full injection)

### 1. Provider Abstraction

**Recommended Pattern**: Trait-based with runtime dispatch (Goose-style factory)

```rust
#[async_trait]
pub trait LLMProvider: Send + Sync {
    async fn chat(&self, messages: Vec<Message>) -> Result<Response>;
    async fn stream(&self, messages: Vec<Message>) -> Result<StreamHandle>;
    fn model_info(&self) -> ModelInfo;
    fn supports_tools(&self) -> bool;
}

pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn LLMProvider>>,
}

impl ProviderRegistry {
    pub fn get(&self, name: &str) -> Option<Arc<dyn LLMProvider>> {
        self.providers.get(name).cloned()
    }
}
```

**Providers to Support**:

1. OpenAI (+ Azure)
2. Anthropic
3. Google (Gemini/Vertex)
4. OpenRouter (aggregator)
5. Local (Ollama, LM Studio via OpenAI-compat)

### 2. Tool System

**Recommended Pattern**: Registry + Orchestrator + MCP

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> &JsonSchema;
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult>;
    fn requires_approval(&self) -> ApprovalLevel;
}

pub struct ToolRegistry {
    builtin: Vec<Arc<dyn Tool>>,
    mcp: Vec<MCPTool>,  // Loaded from MCP servers
}

pub struct ToolOrchestrator {
    registry: ToolRegistry,
    approval_cache: ApprovalCache,
    sandbox: Option<SandboxConfig>,
}

impl ToolOrchestrator {
    pub async fn execute(&self, call: ToolCall, ctx: &Context) -> Result<ToolResult> {
        // 1. Check approval
        // 2. Execute (with sandbox if configured)
        // 3. Handle failure + escalation
        // 4. Return result
    }
}
```

### 3. Plugin/Extension System

**Recommended Pattern**: Skills as markdown prompts

```rust
pub struct Skill {
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub model_override: Option<String>,
    pub prompt: String,  // Loaded from SKILL.md
}

pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    pub fn inject(&self, skill: &Skill, messages: &mut Vec<Message>) {
        // Add meta messages with skill context
        messages.push(Message::meta(skill.prompt.clone()));
    }
}
```

### 4. MCP Integration

**Must-Have**: Full MCP client support

```rust
pub struct MCPClient {
    transport: MCPTransport,  // stdio, SSE, or HTTP
}

impl MCPClient {
    pub async fn list_tools(&self) -> Result<Vec<MCPToolDef>>;
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<Value>;
    pub async fn list_resources(&self) -> Result<Vec<MCPResource>>;
    pub async fn read_resource(&self, uri: &str) -> Result<String>;
}

pub enum MCPTransport {
    Stdio { command: String, args: Vec<String> },
    SSE { url: String },
    HTTP { url: String },
}
```

### 5. Agent Loop

**Recommended Pattern**: Multi-turn with spec-first option

```rust
pub async fn run_task(&mut self, task: &str) -> Result<()> {
    // Optional: Generate spec first (Factory Droid pattern)
    let spec = if self.config.spec_first {
        Some(self.generate_spec(task).await?)
    } else {
        None
    };

    loop {
        let response = self.provider.stream(self.history.clone()).await?;

        if response.is_empty() {
            break;  // Task complete
        }

        for item in response.items {
            match item {
                ResponseItem::Text(text) => self.handle_text(text),
                ResponseItem::ToolCall(call) => {
                    let result = self.orchestrator.execute(call).await?;
                    self.history.push(result);
                }
            }
        }

        // Check token limit, auto-compact if needed
        if self.should_compact() {
            self.compact().await?;
        }
    }

    Ok(())
}
```

---

## Key Tradeoffs

| Decision             | Option A         | Option B        | Recommendation                       |
| -------------------- | ---------------- | --------------- | ------------------------------------ |
| Provider abstraction | SDK (AI SDK)     | Custom traits   | **Custom traits** (Rust control)     |
| Tool execution       | Direct call      | Orchestrator    | **Orchestrator** (approval/sandbox)  |
| Plugin system        | Code plugins     | Markdown skills | **Markdown skills** (safety)         |
| MCP support          | Client only      | Client + Server | **Client first**, server later       |
| Task loop            | Fixed iterations | Until-empty     | **Until-empty** (natural completion) |
| Search               | Regex only       | Regex + Vector  | **Regex primary**, vector fallback   |

---

## References

**Goose** (Primary Reference):

- https://github.com/block/goose
- https://block.github.io/goose/docs/goose-architecture/
- https://block.github.io/goose/docs/guides/context-engineering/using-skills/
- https://dev.to/lymah/deep-dive-into-gooses-extension-system-and-model-context-protocol-mcp-3ehl

**Pi** (Lightweight Reference):

- https://github.com/badlogic/pi-mono
- https://mariozechner.at/posts/2025-11-30-pi-coding-agent/

**OpenCode**:

- https://github.com/opencode-ai/opencode
- https://cefboud.com/posts/coding-agents-internals-opencode-deepdive/

**Claude Code**:

- https://leehanchung.github.io/blogs/2025/10/26/claude-skills-deep-dive/
- https://www.anthropic.com/engineering/claude-code-sandboxing

**Codex CLI**:

- https://github.com/openai/codex
- https://www.philschmid.de/openai-codex-cli

**Gemini CLI**:

- https://github.com/google-gemini/gemini-cli
- https://geminicli.com/docs/architecture/

**Aider**:

- https://github.com/Aider-AI/aider
- https://opendeep.wiki/Aider-AI/aider/technical-reference-architecture

**Factory Droid**:

- https://docs.factory.ai/
- Terminal-Bench leaderboard

**Crush CLI**:

- https://github.com/charmbracelet/crush
