# Bun Migration Strategy

## Overview

Complete rewrite from Python 3.13 to Bun/TypeScript. No incremental migration - full rewrite with feature parity as the goal.

**Pre-migration tag**: `v0.0.1-python`
**Target**: Single-binary distribution via `bun build --compile`

## Component Mapping

### Core Runtime

| Python         | TypeScript   | Notes                     |
| -------------- | ------------ | ------------------------- |
| Python 3.13    | Bun          | 20ms startup vs 100-300ms |
| uv             | bun          | Package management        |
| pyproject.toml | package.json | Project config            |
| pytest         | bun test     | Testing                   |
| ruff           | Biome        | Linting/formatting        |
| ty             | tsc          | Type checking             |

### Agent Framework

| Python         | TypeScript                | Notes                  |
| -------------- | ------------------------- | ---------------------- |
| LangGraph      | Mastra (on Vercel AI SDK) | Agent orchestration    |
| LangChain      | Direct API calls          | Simpler, less overhead |
| Custom routing | Mastra model routing      | Multi-provider support |

**Decision**: Mastra for built-in memory, workflows, and MCP support. See DECISIONS.md (2025-12-18).

### Memory Layer

| Python   | TypeScript        | Notes                         |
| -------- | ----------------- | ----------------------------- |
| DuckDB   | libsql            | 3-6x faster, native vectors   |
| ChromaDB | OmenDB (or Orama) | OmenDB when JS bindings ready |
| NetworkX | Graphology        | Full graph algorithm library  |

### Code Analysis

| Python         | TypeScript            | Notes                      |
| -------------- | --------------------- | -------------------------- |
| py-tree-sitter | web-tree-sitter       | Same grammars work         |
| ast-grep       | @ast-grep/napi        | Rust bindings for Node/Bun |
| Custom LSP     | vscode-languageclient | Mature JS LSP client       |

### TUI

| Python             | TypeScript        | Notes                    |
| ------------------ | ----------------- | ------------------------ |
| (waiting for Toad) | OpenTUI + SolidJS | OpenCode uses same stack |
| Rich               | Ink (fallback)    | React-based terminal     |

### Protocol

| Python          | TypeScript                | Notes        |
| --------------- | ------------------------- | ------------ |
| Custom ACP impl | @agentclientprotocol/sdk  | Official SDK |
| Custom MCP impl | @modelcontextprotocol/sdk | Official SDK |

## Dependency List

### Core

```json
{
  "dependencies": {
    "@agentclientprotocol/sdk": "^0.5",
    "@modelcontextprotocol/sdk": "^1.15",
    "ai": "^4.0",
    "graphology": "^0.25",
    "@libsql/client": "^0.14"
  }
}
```

### TUI

```json
{
  "dependencies": {
    "@opentui/core": "^0.1",
    "@opentui/solid": "^0.1",
    "solid-js": "^1.9"
  }
}
```

### Code Analysis

```json
{
  "dependencies": {
    "web-tree-sitter": "^0.25",
    "@ast-grep/napi": "^0.31"
  }
}
```

### Vector (when ready)

```json
{
  "dependencies": {
    "omendb": "^0.1"
  }
}
```

**Fallback**: `orama` if OmenDB JS bindings delayed

## Directory Structure

```
aircher/
├── src/
│   ├── index.ts           # Entry point
│   ├── agent/
│   │   ├── core.ts        # Agent loop
│   │   ├── intent.ts      # Intent classification
│   │   ├── modes.ts       # READ/WRITE/ADMIN
│   │   └── routing.ts     # Model selection
│   ├── memory/
│   │   ├── integration.ts # Unified facade
│   │   ├── episodic.ts    # libsql layer
│   │   ├── vector.ts      # OmenDB/Orama layer
│   │   ├── graph.ts       # Graphology layer
│   │   └── learned.ts     # .aircher/learned/ management
│   ├── protocol/
│   │   ├── acp.ts         # ACP server
│   │   └── mcp.ts         # MCP client
│   ├── tools/
│   │   ├── index.ts       # Tool registry
│   │   ├── file.ts        # read_file, write_file
│   │   ├── bash.ts        # Shell execution
│   │   └── search.ts      # Grep, glob, semantic
│   ├── analysis/
│   │   ├── parser.ts      # tree-sitter wrapper
│   │   ├── ast.ts         # AST operations
│   │   └── lsp.ts         # LSP client
│   ├── tui/
│   │   ├── app.tsx        # SolidJS app
│   │   ├── input.tsx      # Input component
│   │   ├── output.tsx     # Streaming output
│   │   └── status.tsx     # Status bar
│   └── config/
│       ├── settings.ts    # Configuration
│       └── project.ts     # Project detection
├── tests/
│   ├── agent/
│   ├── memory/
│   ├── tools/
│   └── integration/
├── scripts/
│   └── build.ts           # Build script
├── package.json
├── tsconfig.json
├── biome.json
└── bunfig.toml
```

## Migration Phases

### 8.1 Project Scaffolding

```bash
bun init
bun add typescript @types/bun
```

**Output**: Buildable empty project with CI.

**Validation**:

- `bun test` runs (empty)
- `bun build` succeeds
- GitHub Actions green

### 8.2 TUI Shell

Start with minimal TUI before agent logic:

```typescript
// src/tui/app.tsx
import { render } from "@opentui/solid";
import { createSignal } from "solid-js";

function App() {
  const [output, setOutput] = createSignal<string[]>([]);
  return (
    <box>
      <output lines={output()} />
      <input onSubmit={(text) => setOutput(o => [...o, text])} />
    </box>
  );
}

render(App);
```

**Output**: TUI that echoes input.

**Validation**:

- Can type commands
- Displays output
- /quit exits

### 8.3 Agent Core

Implement agent loop without memory:

```typescript
// src/agent/core.ts
import { generateText } from "ai";
import { anthropic } from "@ai-sdk/anthropic";

export async function runAgent(prompt: string) {
  const { text } = await generateText({
    model: anthropic("claude-sonnet-4-20250514"),
    prompt,
    tools: { read_file, write_file, bash },
  });
  return text;
}
```

**Output**: Working agent with basic tools.

**Validation**:

- Can answer questions
- Can read files
- Can execute commands

### 8.4 Memory Layer

Add 2-layer memory (episodic + graph):

```typescript
// src/memory/integration.ts
export class MemoryIntegration {
  constructor(
    private episodic: LibSQLMemory,
    private graph: GraphologyMemory,
  ) {}

  async recordToolExecution(tool: string, args: any, result: any) {
    await this.episodic.insert({
      type: "tool_execution",
      tool,
      args,
      result,
      timestamp: Date.now(),
    });
  }

  async getRelevantContext(query: string) {
    const recent = await this.episodic.getRecent(20);
    const related = this.graph.findRelated(query);
    return { recent, related };
  }
}
```

**Output**: Agent remembers across sessions.

**Validation**:

- Tool executions persisted
- Context retrieved on restart
- Graph updated from tree-sitter

### 8.5 Vector Memory

Add semantic search when OmenDB ready:

```typescript
// src/memory/vector.ts
export class VectorMemory {
  private db: OmenDB;

  async search(query: string, k = 10) {
    const embedding = await getEmbedding(query);
    return this.db.search(embedding, k);
  }

  async index(content: string, metadata: any) {
    const embedding = await getEmbedding(content);
    await this.db.insert({ embedding, metadata });
  }
}
```

**Fallback**: Use Orama if OmenDB delayed.

**Validation**:

- Semantic search works
- Hybrid search (keyword + semantic) works

### 8.6 ACP Server

Expose agent via ACP:

```typescript
// src/protocol/acp.ts
import { ACPServer } from "@agentclientprotocol/sdk";

export function createACPServer(agent: AircherAgent) {
  return new ACPServer({
    name: "aircher",
    version: "0.1.0",
    capabilities: {
      tools: true,
      memory: true,
    },
    handlers: {
      async runTask(task) {
        return agent.run(task);
      },
    },
  });
}
```

**Output**: Works with Toad, Zed, Neovim.

**Validation**:

- Toad can connect
- Sessions work
- Tools exposed

### 8.7 Feature Parity

Checklist from Python version:

| Feature                | Python            | TypeScript      |
| ---------------------- | ----------------- | --------------- |
| Episodic memory        | DuckDB            | libsql          |
| Vector search          | ChromaDB          | OmenDB/Orama    |
| Knowledge graph        | NetworkX          | Graphology      |
| Intent classification  | LangGraph         | Custom          |
| Model routing          | Custom            | AI SDK          |
| Dynamic context        | Custom            | Custom          |
| Learned context        | .aircher/learned/ | Same            |
| ACP protocol           | Custom            | Official SDK    |
| LSP validation         | pylsp             | vscode-lsp      |
| Tree-sitter parsing    | py-tree-sitter    | web-tree-sitter |
| READ/WRITE/ADMIN modes | Click             | Custom          |

### 8.8 Distribution

```bash
bun build ./src/index.ts --compile --outfile aircher
```

**Targets**:

- `aircher-darwin-arm64`
- `aircher-darwin-x64`
- `aircher-linux-x64`
- `aircher-linux-arm64`
- `aircher-windows-x64`

**Distribution**:

- GitHub releases (binaries)
- npm package
- Homebrew formula

## Risk Mitigation

| Risk                          | Mitigation                      |
| ----------------------------- | ------------------------------- |
| OmenDB JS bindings delayed    | Use Orama as fallback           |
| OpenTUI too complex           | Start with Ink, migrate later   |
| libsql missing features       | Raw SQLite via bun:sqlite       |
| Graphology missing algorithms | Implement specific ones needed  |
| Migration scope creep         | Strict feature parity checklist |

## Success Criteria

- [ ] All Python features reimplemented
- [ ] Single binary under 50MB
- [ ] Startup < 50ms
- [ ] Memory queries < 100ms
- [ ] ACP compatibility with Toad
- [ ] All tests passing
- [ ] Documentation updated

## Reference Implementations

| Project     | Stack                   | Notes             |
| ----------- | ----------------------- | ----------------- |
| OpenCode    | Bun + SolidJS + OpenTUI | Direct reference  |
| Claude Code | Bun + TypeScript        | Anthropic's agent |
| Cline       | Node + TypeScript       | Popular extension |
| Gemini CLI  | Node + TypeScript       | Google's agent    |

## Timeline

No time estimates. Work in order:

1. Scaffolding (8.1)
2. TUI (8.2)
3. Agent (8.3)
4. Memory (8.4)
5. Vectors (8.5)
6. ACP (8.6)
7. Parity (8.7)
8. Distribution (8.8)

Each phase validated before next. Python code archived as reference.
