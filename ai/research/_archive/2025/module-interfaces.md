# Module Interfaces Specification

**Purpose**: Define TypeScript interfaces before implementation. All code derives from these.

## Directory Structure

```
src/
├── index.ts              # Entry point (TUI or ACP based on args)
├── types.ts              # Shared types (re-export from modules)
├── config.ts             # Configuration loading
├── agent/
│   ├── index.ts          # Export Agent
│   ├── agent.ts          # AircherAgent class
│   ├── tools.ts          # Tool definitions for Mastra
│   └── prompts.ts        # System prompts
├── memory/
│   ├── index.ts          # Export MemoryIntegration
│   ├── integration.ts    # Unified facade
│   ├── episodic.ts       # libsql layer
│   ├── vector.ts         # OmenDB/Orama layer
│   ├── graph.ts          # Graphology layer
│   └── learned.ts        # .aircher/learned/ management
├── protocol/
│   ├── index.ts          # Export ACPServer
│   ├── acp.ts            # ACP server implementation
│   └── mcp.ts            # MCP client for tool servers
├── analysis/
│   ├── index.ts          # Export CodeAnalyzer
│   ├── parser.ts         # tree-sitter wrapper
│   └── lsp.ts            # LSP client
└── tui/
    ├── index.ts          # Export startTui
    ├── app.tsx           # SolidJS app root
    └── components/       # UI components
```

## Core Interfaces

### Agent

```typescript
// src/agent/agent.ts
import { Agent } from "@mastra/core";

interface AgentConfig {
  model: string; // e.g., "deepseek/deepseek-v3.2-speciale"
  memory: MemoryIntegration;
  mode: AgentMode;
}

type AgentMode = "read" | "write" | "admin";

interface RunOptions {
  prompt: string;
  context?: RelevantContext;
  onStream?: (chunk: string) => void;
}

interface RunResult {
  text: string;
  toolCalls: ToolCall[];
  cost: CostInfo;
}

// Using Mastra Agent
export class AircherAgent {
  private mastraAgent: Agent;
  private memory: MemoryIntegration;
  private mode: AgentMode;

  static async create(config: AgentConfig): Promise<AircherAgent>;
  async run(options: RunOptions): Promise<RunResult>;
  async runWithWorkflow(workflowId: string, input: unknown): Promise<RunResult>;
}
```

### Memory

```typescript
// src/memory/integration.ts
interface MemoryConfig {
  projectRoot: string;
  globalDbPath: string; // ~/.aircher/data/global.db
  projectDbPath: string; // ~/.aircher/data/projects/{hash}/
}

interface AgentEvent {
  timestamp: number;
  type:
    | "user_message"
    | "assistant_message"
    | "tool_call"
    | "tool_result"
    | "error";
  content: unknown;
  metadata?: Record<string, unknown>;
}

interface RelevantContext {
  recentEvents: AgentEvent[];
  relatedCode: CodeSnippet[];
  learnedPatterns: string[];
}

export class MemoryIntegration {
  private episodic: EpisodicMemory;
  private vector: VectorMemory;
  private graph: GraphMemory;
  private learned: LearnedContext;

  static async initialize(config: MemoryConfig): Promise<MemoryIntegration>;

  // Recording
  async record(event: AgentEvent): Promise<void>;
  async recordToolExecution(
    tool: string,
    args: unknown,
    result: unknown,
  ): Promise<void>;

  // Retrieval
  async getRelevantContext(query: string): Promise<RelevantContext>;
  async searchSemantic(query: string, k?: number): Promise<CodeSnippet[]>;

  // Agent self-edit tools (Letta-inspired)
  async memoryNote(fact: string): Promise<void>;
  async memorySearch(query: string): Promise<MemorySearchResult>;
  async forget(id: string): Promise<void>;
}
```

### Episodic Memory (libsql)

```typescript
// src/memory/episodic.ts
import { Client } from "@libsql/client";

interface EpisodicConfig {
  dbPath: string;
}

export class EpisodicMemory {
  private db: Client;

  static async create(config: EpisodicConfig): Promise<EpisodicMemory>;

  async insert(event: AgentEvent): Promise<string>; // Returns event ID
  async getRecent(limit: number): Promise<AgentEvent[]>;
  async getByType(
    type: AgentEvent["type"],
    limit: number,
  ): Promise<AgentEvent[]>;
  async search(query: string): Promise<AgentEvent[]>; // Full-text search
}
```

### Vector Memory

```typescript
// src/memory/vector.ts
interface VectorConfig {
  dbPath: string;
  embeddingModel: string; // Default: local embedding
}

interface CodeSnippet {
  path: string;
  content: string;
  startLine: number;
  endLine: number;
  score: number;
}

export class VectorMemory {
  static async create(config: VectorConfig): Promise<VectorMemory>;

  async index(
    content: string,
    metadata: { path: string; lines: [number, number] },
  ): Promise<void>;
  async search(query: string, k?: number): Promise<CodeSnippet[]>;
  async remove(path: string): Promise<void>; // Remove file from index
}
```

### Graph Memory (Graphology)

```typescript
// src/memory/graph.ts
import Graph from "graphology";

interface CodeNode {
  id: string; // file:symbol format
  type: "file" | "function" | "class" | "variable";
  name: string;
  path: string;
  line: number;
}

interface CodeEdge {
  type: "imports" | "calls" | "extends" | "uses";
}

export class GraphMemory {
  private graph: Graph;

  static create(): GraphMemory;

  // Building
  addNode(node: CodeNode): void;
  addEdge(source: string, target: string, edge: CodeEdge): void;
  updateFromAST(path: string, ast: unknown): void; // tree-sitter AST

  // Querying
  findRelated(query: string): CodeNode[];
  getDependencies(nodeId: string): CodeNode[];
  getReferences(nodeId: string): CodeNode[];
  getCallGraph(functionId: string, depth?: number): Graph;

  // Persistence
  export(): object;
  import(data: object): void;
}
```

### Learned Context

```typescript
// src/memory/learned.ts
interface LearnedConfig {
  projectRoot: string; // .aircher/learned/
}

interface LearnedPattern {
  type: "pattern" | "error" | "preference";
  content: string;
  confidence: number;
  timestamp: number;
}

export class LearnedContext {
  private basePath: string;

  static create(config: LearnedConfig): LearnedContext;

  // Files: patterns.md, errors.md, context.md
  async updatePatterns(patterns: string[]): Promise<void>;
  async recordErrorFix(error: string, fix: string): Promise<void>;
  async addContext(key: string, value: string): Promise<void>;

  // Retrieval
  async getCombinedContext(): Promise<string>; // Concatenated for prompt
  async getPatterns(): Promise<LearnedPattern[]>;
}
```

### Protocol (ACP)

```typescript
// src/protocol/acp.ts
import { ACPServer as BaseACPServer } from "@agentclientprotocol/sdk";

interface ACPConfig {
  name: string;
  version: string;
  agent: AircherAgent;
}

export class ACPServer {
  private server: BaseACPServer;
  private agent: AircherAgent;

  static create(config: ACPConfig): ACPServer;

  // Lifecycle
  async start(): Promise<void>; // stdio transport
  async stop(): Promise<void>;

  // Handlers (internal, called by SDK)
  private handleRunTask(task: ACPTask): Promise<ACPResponse>;
  private handleListTools(): Promise<ACPTool[]>;
}
```

### Tools

```typescript
// src/agent/tools.ts
import { createTool } from "@mastra/core";
import { z } from "zod";

// Core tools exposed to agent
export const readFile = createTool({
  id: "read_file",
  description: "Read a file from the filesystem",
  inputSchema: z.object({
    path: z.string(),
    startLine: z.number().optional(),
    endLine: z.number().optional(),
  }),
  execute: async ({ context, input }) => {
    /* ... */
  },
});

export const writeFile = createTool({
  id: "write_file",
  description: "Write content to a file",
  inputSchema: z.object({
    path: z.string(),
    content: z.string(),
  }),
  execute: async ({ context, input }) => {
    /* ... */
  },
});

export const bash = createTool({
  id: "bash",
  description: "Execute a shell command",
  inputSchema: z.object({
    command: z.string(),
    timeout: z.number().optional().default(30000),
  }),
  execute: async ({ context, input }) => {
    /* ... */
  },
});

export const glob = createTool({
  id: "glob",
  description: "Find files matching a pattern",
  inputSchema: z.object({
    pattern: z.string(),
    cwd: z.string().optional(),
  }),
  execute: async ({ context, input }) => {
    /* ... */
  },
});

export const grep = createTool({
  id: "grep",
  description: "Search for pattern in files",
  inputSchema: z.object({
    pattern: z.string(),
    path: z.string().optional(),
    type: z.string().optional(), // File type filter
  }),
  execute: async ({ context, input }) => {
    /* ... */
  },
});

// Memory self-edit tools (Letta-inspired)
export const memoryNote = createTool({
  id: "memory_note",
  description: "Record a fact to persistent memory",
  inputSchema: z.object({
    fact: z.string(),
    type: z.enum(["pattern", "error", "preference"]).optional(),
  }),
  execute: async ({ context, input }) => {
    /* ... */
  },
});

export const memorySearch = createTool({
  id: "memory_search",
  description: "Search memory for relevant information",
  inputSchema: z.object({
    query: z.string(),
  }),
  execute: async ({ context, input }) => {
    /* ... */
  },
});
```

### Configuration

```typescript
// src/config.ts
interface AircherConfig {
  // Model
  model: string;
  apiKey?: string;
  baseUrl?: string;

  // Memory
  globalDataPath: string; // ~/.aircher/
  projectDataPath?: string; // Auto-detected from .git

  // Agent
  mode: AgentMode;
  maxTokens: number;
  temperature: number;

  // TUI
  tuiEnabled: boolean;
}

export function loadConfig(): AircherConfig;
export function getProjectHash(root: string): string;
```

## Implementation Order

1. **Types & Config** (src/types.ts, src/config.ts)
2. **Memory Layer** (src/memory/) - Core persistence
3. **Agent** (src/agent/) - Mastra integration
4. **Protocol** (src/protocol/) - ACP server
5. **TUI** (src/tui/) - OpenTUI interface

Each module should be independently testable before integration.

## Validation Criteria

| Module | Validation                     |
| ------ | ------------------------------ |
| Memory | Events persist across restarts |
| Agent  | Tool calls execute correctly   |
| ACP    | Toad can connect and run tasks |
| TUI    | Input/output works             |

## References

- DESIGN.md: Architecture overview
- DECISIONS.md: Framework decisions (Mastra, ACP-first)
- bun-migration.md: Migration phases
- context-compaction.md: Memory management patterns
