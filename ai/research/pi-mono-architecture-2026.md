# Pi-Mono Architecture Analysis 2026

**Research Date**: 2026-01-25
**Purpose**: Reference architecture for plugin system, skills, and extensibility
**Source**: https://github.com/badlogic/pi-mono

---

## Executive Summary

Pi-mono is a TypeScript monorepo implementing a terminal-based coding agent with strong emphasis on extensibility over opinionated defaults. Key differentiators: no MCP (by design), modular package architecture, powerful extension/plugin system, session branching, and package distribution mechanism.

**Relevant for ion**:

- Extension/plugin architecture patterns
- Skills system (follows agentskills.io spec)
- Session tree structure with branching
- Package manager for distributing customizations
- Context compaction approach

---

## Package Architecture

```
pi-mono/
├── packages/
│   ├── ai/           # LLM provider abstraction (16+ providers)
│   ├── agent/        # Core agent loop and state machine
│   ├── coding-agent/ # CLI + TUI implementation
│   ├── tui/          # Terminal rendering framework
│   ├── web-ui/       # Reusable web components
│   ├── mom/          # Slack integration
│   └── pods/         # vLLM deployment orchestration
```

### Layer Separation

| Layer       | Package           | Purpose                              |
| ----------- | ----------------- | ------------------------------------ |
| Foundation  | `pi-ai`           | Unified LLM API across 16+ providers |
| Core        | `pi-agent`        | Agent loop, tool execution, events   |
| Application | `pi-coding-agent` | CLI, TUI, extensions, skills         |
| Rendering   | `pi-tui`          | Differential terminal rendering      |

---

## Core Agent (`packages/agent`)

```
src/
├── agent.ts        # Main Agent class
├── agent-loop.ts   # Turn-based execution loop
├── proxy.ts        # Proxy utilities
├── types.ts        # Core type definitions
└── index.ts        # Exports
```

### Agent State Machine

```typescript
const agent = new Agent({
  initialState: {
    systemPrompt: "...",
    model: getModel("anthropic", "claude-sonnet-4"),
  },
});

// State management
agent.setSystemPrompt("...");
agent.setModel(model);
agent.setThinkingLevel("medium");
agent.setTools([...]);
agent.replaceMessages([...]);
agent.reset();

// Control flow
agent.steer(message);   // Interrupt current execution
agent.followUp(message); // Queue after completion
```

### Event System

Events enable UI integration and extension hooks:

| Event                                         | Purpose                 |
| --------------------------------------------- | ----------------------- |
| `agent_start` / `agent_end`                   | Full processing cycle   |
| `turn_start` / `turn_end`                     | Single LLM call + tools |
| `message_update`                              | Streaming text chunks   |
| `tool_execution_start` / `tool_execution_end` | Tool lifecycle          |

---

## Coding Agent Structure (`packages/coding-agent`)

```
src/
├── cli/
│   ├── args.ts              # CLI argument parsing
│   ├── config-selector.ts   # Config selection
│   ├── file-processor.ts    # File processing
│   ├── list-models.ts       # Model listing
│   └── session-picker.ts    # Session selection
├── core/
│   ├── agent-session.ts     # Session management
│   ├── extensions/          # Extension system
│   ├── tools/               # Built-in tools
│   ├── compaction/          # Context compaction
│   ├── skills.ts            # Skills loader
│   ├── event-bus.ts         # Internal events
│   ├── package-manager.ts   # Package installation
│   ├── settings-manager.ts  # Config persistence
│   ├── session-manager.ts   # Session persistence
│   └── model-registry.ts    # Model management
├── modes/
│   ├── interactive/         # TUI mode
│   ├── rpc/                 # External integration
│   └── print-mode.ts        # Non-interactive output
└── utils/
```

---

## Extension System

### Extension Types

Extensions in `~/.pi/agent/extensions/` can provide:

1. **Lifecycle Event Handlers** - Hook into agent/session events
2. **Custom Tools** - New capabilities beyond built-in tools
3. **Commands** - Custom slash commands
4. **Keyboard Shortcuts** - Custom keybindings
5. **UI Components** - Headers, footers, overlays
6. **Custom Providers** - New LLM integrations

### Extension API

```typescript
interface ExtensionAPI {
  // Event subscription
  on(event: ExtensionEventType, handler: Handler): () => void;

  // Registration
  registerTool(tool: ToolDefinition): void;
  registerCommand(name: string, handler: CommandHandler): void;
  registerShortcut(key: string, handler: ShortcutHandler): void;

  // Session management
  getSession(): AgentSession;
  getModel(): Model;
  selectModel(model: Model): void;

  // UI capabilities
  showNotification(message: string): void;
  showDialog(options: DialogOptions): Promise<Result>;
}
```

### Extension Context

```typescript
interface ExtensionContext {
  // UI interaction
  ui: ExtensionUIContext;

  // Session access (read-only)
  session: AgentSession;
  model: Model;

  // Agent control
  isIdle(): boolean;
  abort(): void;
  getContextUsage(): ContextUsage;
}
```

### Extension Events

**Session Events**:

- `session_start`, `session_before_switch`, `session_fork`
- `session_before_compact`, `session_shutdown`, `session_before_tree`

**Agent Events**:

- `before_agent_start`, `agent_start`, `agent_end`
- `turn_start`, `turn_end`

**Tool Events**:

- `tool_call`, `tool_result`
- Type-specific: `bash`, `read`, `edit`, `grep`, `find`, `ls`

**Input Events**:

- `input`, `user_bash`, `model_select`, `context`

Many "before" events support cancellation/modification.

### Extension Loading

```
extensions/
├── loader.ts   # Discovery and initialization
├── runner.ts   # Execution and lifecycle
├── wrapper.ts  # Isolation/integration
└── types.ts    # Interface definitions
```

---

## Skills System

Follows the agentskills.io specification.

### Skill Structure

```
~/.pi/agent/skills/
└── my-skill/
    └── SKILL.md    # Skill definition
```

### Skill Interface

```typescript
interface Skill {
  name: string; // Matches directory name
  description: string; // Purpose (max 1024 chars)
  filePath: string; // Location on disk
  baseDir: string; // Skill directory
  source: "user" | "project" | "path";
  disableModelInvocation?: boolean;
}
```

### Skill Loading

Three sources, with precedence:

1. User global: `~/.pi/agent/skills/`
2. Project local: `.pi/skills/`
3. Explicit paths via options

**Validation Rules**:

- Name: lowercase a-z, 0-9, hyphens only
- No leading/trailing hyphens
- Must match parent directory name
- Max 64 characters

Collisions generate diagnostics identifying which skill wins.

---

## Package Distribution

### Pi Packages

Distribute customizations via npm or git:

```json
{
  "pi": {
    "extensions": ["./extensions"],
    "skills": ["./skills"],
    "prompts": ["./prompts"],
    "themes": ["./themes"]
  }
}
```

### Package Manager Commands

```bash
pi install <package>  # npm package or git URL
pi remove <package>
pi list
pi update
```

### Resource Discovery

Two mechanisms:

1. **Convention**: Standard directories (`extensions/`, `skills/`, etc.)
2. **Manifest**: Explicit `pi` field with glob patterns

Scoping: project scope wins over global for same package identity.

---

## Session Management

### Session Format

**JSONL files** with tree structure:

```jsonl
{"type": "header", "id": "...", "timestamp": "...", "workdir": "..."}
{"type": "message", "id": "...", "parentId": "...", "role": "user", ...}
{"type": "message", "id": "...", "parentId": "...", "role": "assistant", ...}
```

### Tree Structure

Each entry has `id` and `parentId`, enabling:

- In-place branching (no history modification)
- Session forking
- Tree navigation

```
root
├── msg1 ─ msg2 ─ msg3 (current leaf)
│         └── msg4 ─ msg5 (branch)
└── msg6 (different branch from root)
```

### Session Operations

| Command    | Effect                             |
| ---------- | ---------------------------------- |
| `/tree`    | Navigate session tree              |
| `/fork`    | Create branch from current point   |
| `/compact` | Compress context, preserve summary |
| `/resume`  | Resume previous session            |

### Context Resolution

`buildSessionContext()` walks from current leaf to root:

- Collects messages along path
- Handles compaction summaries
- Resolves branch context

---

## Context Compaction

```
compaction/
├── compaction.ts          # Core logic
├── branch-summarization.ts # Branch summary generation
├── utils.ts               # Helpers
└── index.ts
```

**Approach**: Generate summaries of conversation branches to reduce token usage while preserving semantic meaning.

---

## Built-in Tools

| Tool    | Purpose                   |
| ------- | ------------------------- |
| `read`  | File reading              |
| `write` | File creation             |
| `edit`  | Surgical text replacement |
| `bash`  | Shell execution           |
| `grep`  | Content search            |
| `find`  | File discovery            |
| `ls`    | Directory listing         |

### Edit Tool Design

**Matching strategy**: Exact match first, fuzzy fallback.

**Key behaviors**:

- Uniqueness validation: Rejects if multiple matches found
- Line ending normalization (LF internally, preserves original)
- BOM stripping before matching
- Abort signal handling at multiple checkpoints
- Pluggable operations interface (for SSH/cloud editing)

---

## TUI Architecture (`packages/tui`)

### Core Components

```typescript
interface TUIComponent {
  render?(width: number): string[];
  handleInput?(data: string): boolean;
  invalidate?(): void;
}
```

### Rendering

Three-strategy differential rendering:

1. Full repaint (initial)
2. Line-by-line diff (typical)
3. Incremental update (minimal changes)

### Built-in Components

`Container`, `Box`, `Text`, `Input`, `Editor`, `Markdown`, `SelectList`, `Image`, `Loader`

### Advanced Features

- **Overlays**: Position components over existing content
- **IME Support**: CJK input method support
- **Synchronized Output**: CSI 2026 protocol for flicker-free updates
- **Paste Detection**: Large paste marking

---

## LLM Provider Abstraction (`packages/ai`)

### Unified API

```typescript
// Type-safe model discovery
const model = getModel("anthropic", "claude-sonnet-4");

// Unified streaming
const events = stream({
  model,
  context: { systemPrompt, messages, tools },
});

// Or non-streaming
const response = await complete({ model, context });
```

### Supported Providers (16+)

Anthropic, OpenAI, Azure OpenAI, Google Gemini, Vertex AI, Amazon Bedrock, Mistral, Groq, OpenAI-compatible (Ollama, vLLM, LM Studio)

### Tool Abstraction

TypeBox schemas provide portable tool definitions that serialize to JSON.

---

## RPC Mode

For external integration without interactive TUI:

```
rpc/
├── rpc-mode.ts    # Core logic
├── rpc-client.ts  # Client implementation
└── rpc-types.ts   # Protocol types
```

Enables `pi --mode rpc` for stdin/stdout integration with non-Node systems.

---

## Configuration Hierarchy

### User Config Paths

```
~/.pi/agent/
├── models.json     # Model definitions
├── auth.json       # Credentials
├── settings.json   # Preferences
├── themes/         # Custom themes
├── tools/          # Custom tools
├── prompts/        # Prompt templates
└── sessions/       # Session storage
```

### Project Config

- `.pi/SYSTEM.md` - Custom system prompt
- `AGENTS.md` - Project context (walked up from cwd)

### Loading Pattern

Discovery-based: built-in and user configs exist in separate directories, selected by context rather than merged.

---

## Design Philosophy

### Explicit Non-Features

Pi intentionally omits:

- **MCP integration** - Build via extensions instead
- **Sub-agents** - Spawn instances or extend
- **Plan mode** - Write to files or customize
- **Background bash** - Use tmux
- **Built-in permission popups** - Containerize or extend

This prioritizes extensibility over opinionated defaults.

---

## Takeaways for ion

### Worth Adopting

1. **Extension event system** - Before/after hooks with cancellation
2. **Skills spec compliance** - agentskills.io format with validation rules
3. **Session tree structure** - Branching without history modification
4. **Package distribution** - npm/git packages with manifest
5. **Edit tool design** - Exact match + fuzzy fallback, uniqueness validation

### Consider Adapting

1. **Differential TUI rendering** - Three-strategy approach
2. **Context compaction** - Branch summarization
3. **RPC mode** - stdin/stdout for external integration

### Avoid/Different Approach

1. **No MCP** - ion should support MCP client
2. **TypeScript ecosystem** - ion is Rust, different extension patterns needed
3. **Monorepo structure** - ion is single crate

---

## References

- Repository: https://github.com/badlogic/pi-mono
- Coding Agent: https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent
- Skills Spec: https://agentskills.io
