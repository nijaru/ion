# API Design Specification

## Overview

This document specifies the public API for aircher. The API is designed to be simple for common use cases while providing flexibility for advanced usage.

## Design Principles

1. **Simple defaults** - Works out of the box
2. **Progressive disclosure** - Advanced features available but not required
3. **Type-safe** - Full TypeScript support
4. **Composable** - Mix and match components

## Core API

### Memory Manager

The main entry point for memory operations:

```typescript
import { Memory } from "aircher";

// Simple initialization (uses coding archetype by default)
const memory = new Memory({
  projectPath: "/path/to/project",
});

// With options
const memory = new Memory({
  projectPath: "/path/to/project",
  archetype: "coding", // or custom archetype object
  dataDir: "~/.aircher/data",
  embedder: "openai", // or 'ollama', or custom embedder
});

// Record an event
await memory.record({
  type: "file_read",
  content: { path: "src/index.ts", text: "..." },
});

// Build context for a prompt
const context = await memory.recall({
  query: "fix the auth bug",
  maxTokens: 8000,
});

// Persist changes
await memory.persist();

// Close (call when done)
await memory.close();
```

### Memory Interface

```typescript
interface Memory {
  // Configuration
  readonly projectPath: string;
  readonly archetype: Archetype;
  readonly sessionId: string;

  // Core operations
  record(event: EventInput): Promise<Event>;
  recall(request: RecallRequest): Promise<Context>;

  // Direct access to stores (advanced)
  readonly episodic: EpisodicStore;
  readonly semantic: SemanticStore;
  readonly procedural: ProceduralStore;
  readonly relational: RelationalStore;
  readonly contextual: VectorStore;
  readonly context: ContextBuilder;

  // Lifecycle
  persist(): Promise<void>;
  close(): Promise<void>;
}

interface EventInput {
  type: string;
  content: unknown;
  outcome?: "success" | "failure" | "partial";
  parentId?: string;
}

interface RecallRequest {
  query: string;
  maxTokens?: number; // Default: 8000
  strategy?: Partial<ContextStrategy>;
  filters?: ContextFilters;
}
```

## Store APIs

### Episodic Store

```typescript
interface EpisodicStore {
  // Record events
  record(event: Event): Promise<void>;
  recordBatch(events: Event[]): Promise<void>;

  // Query events
  get(id: string): Promise<Event | null>;
  getRecent(sessionId: string, limit?: number): Promise<Event[]>;
  getByType(type: string, limit?: number): Promise<Event[]>;
  getChain(eventId: string): Promise<Event[]>;

  // Search
  search(query: string, options?: SearchOptions): Promise<Event[]>;

  // Sessions
  getRecentSessions(limit?: number): Promise<string[]>;
  getSessionHighlights(sessionIds: string[]): Promise<Event[]>;
}
```

### Semantic Store

```typescript
interface SemanticStore {
  // Manage entities
  upsert(entity: Entity): Promise<void>;
  get(id: string): Promise<Entity | null>;
  findByName(name: string): Promise<Entity | null>;
  getByType(type: string, limit?: number): Promise<Entity[]>;

  // Salience
  getSalient(limit?: number): Promise<Entity[]>;
  updateSalience(id: string, salience: number): Promise<void>;
}
```

### Procedural Store

```typescript
interface ProceduralStore {
  // Manage patterns
  record(pattern: Pattern): Promise<void>;
  get(id: string): Promise<Pattern | null>;

  // Pattern matching
  match(context: MatchContext): Promise<Pattern[]>;

  // Track usage
  applyResult(patternId: string, success: boolean): Promise<void>;

  // Query
  getBest(type: string, limit?: number): Promise<Pattern[]>;
}
```

### Relational Store

```typescript
interface RelationalStore {
  // Manage relations
  add(relation: Relation): Promise<void>;
  get(id: string): Promise<Relation | null>;
  remove(id: string): Promise<void>;

  // Query
  getRelations(entityId: string): Promise<Relation[]>;
  getRelationsByType(type: string): Promise<Relation[]>;

  // Graph operations
  traverse(startId: string, depth: number): Promise<TraversalResult>;
  findPath(sourceId: string, targetId: string): Promise<string[]>;
  getNeighbors(entityId: string, types?: string[]): Promise<Entity[]>;
}
```

### Vector Store

```typescript
interface VectorStore {
  // Index content
  index(chunk: ContentChunk): Promise<void>;
  indexBatch(chunks: ContentChunk[]): Promise<void>;

  // Search
  search(query: string, k?: number): Promise<SearchResult[]>;
  searchByVector(embedding: number[], k?: number): Promise<SearchResult[]>;
  hybridSearch(query: string, filters?: Filter[]): Promise<SearchResult[]>;

  // Maintenance
  delete(id: string): Promise<void>;
  deleteBatch(ids: string[]): Promise<void>;
  optimize(): Promise<void>;
}
```

## Agent API

For building agents on top of memory:

```typescript
import { Agent, Memory } from "aircher";
import { codingTools } from "aircher/archetypes/code";

// Create agent with memory
const agent = new Agent({
  memory: new Memory({ projectPath: "." }),
  tools: codingTools,
  model: "deepseek/deepseek-v3.2-speciale",
});

// Run agent
const result = await agent.run("fix the auth bug");
console.log(result.response);

// Access conversation
const messages = agent.getMessages();

// Clear conversation (keeps memory)
agent.clearConversation();
```

### Agent Interface

```typescript
interface Agent {
  readonly memory: Memory;
  readonly tools: Tool[];

  // Run agent
  run(prompt: string, options?: RunOptions): Promise<AgentResult>;

  // Conversation management
  getMessages(): Message[];
  clearConversation(): void;
  addMessage(message: Message): void;

  // Lifecycle
  close(): Promise<void>;
}

interface RunOptions {
  maxSteps?: number; // Default: 20
  maxTokens?: number; // Context budget
  onStep?: (step: Step) => void;
  signal?: AbortSignal;
}

interface AgentResult {
  response: string;
  steps: Step[];
  usage: {
    promptTokens: number;
    completionTokens: number;
    totalCost: number;
  };
}
```

## Archetype API

For creating custom archetypes:

```typescript
import { createArchetype, extendArchetype, codingArchetype } from "aircher";

// Create from scratch
const myArchetype = createArchetype({
  name: "my-agent",
  description: "My custom agent",
  eventTypes: ["action", "observation"],
  entityTypes: ["item"],
  relationTypes: ["related_to"],
  patternTypes: ["workflow"],
  extractors: {
    /* ... */
  },
  contextStrategy: {
    /* ... */
  },
  tools: [],
});

// Extend existing
const customCoding = extendArchetype(codingArchetype, {
  name: "custom-coding",
  eventTypes: [...codingArchetype.eventTypes, "deploy"],
  extractors: {
    ...codingArchetype.extractors,
    deploy: (event) => [
      /* ... */
    ],
  },
});
```

## Tool Definitions

Standard tool interface:

```typescript
interface Tool {
  name: string;
  description: string;
  parameters: ToolParameters;
  execute: (params: Record<string, unknown>) => Promise<ToolResult>;
}

interface ToolParameters {
  type: "object";
  properties: Record<string, ParameterSchema>;
  required?: string[];
}

interface ToolResult {
  success: boolean;
  output: unknown;
  error?: string;
}
```

### Built-in Tools (Coding Archetype)

```typescript
import { codingTools } from "aircher/archetypes/code";

// Available tools:
// - read_file: Read file contents
// - write_file: Write file contents
// - edit_file: Edit file with diff
// - bash: Execute shell command
// - glob: Find files by pattern
// - grep: Search file contents

// Use in agent
const agent = new Agent({
  memory,
  tools: codingTools,
  model: "deepseek/deepseek-v3.2-speciale",
});

// Or use individually
import { readFile, bash } from "aircher/archetypes/code/tools";

const content = await readFile.execute({ path: "src/index.ts" });
const result = await bash.execute({ command: "npm test" });
```

## Configuration

### Environment Variables

```bash
# Model configuration
AIRCHER_MODEL=deepseek/deepseek-v3.2-speciale
OPENROUTER_API_KEY=...

# Embedding configuration
AIRCHER_EMBEDDER=openai  # or 'ollama'
OPENAI_API_KEY=...       # for openai embeddings
OLLAMA_BASE_URL=...      # for ollama embeddings

# Data storage
AIRCHER_DATA_DIR=~/.aircher/data

# Debug
AIRCHER_DEBUG=true
AIRCHER_LOG_LEVEL=debug
```

### Configuration File

```typescript
// aircher.config.ts
import type { AircherConfig } from "aircher";

export default {
  model: "deepseek/deepseek-v3.2-speciale",
  embedder: "openai",
  dataDir: "~/.aircher/data",

  // Default context strategy
  contextStrategy: {
    recencyWeight: 0.3,
    relevanceWeight: 0.5,
    salientWeight: 0.2,
    depthLimit: 2,
  },

  // Token budget
  maxContextTokens: 8000,
} satisfies AircherConfig;
```

## ACP Server API

For editor integration:

```typescript
import { createACPServer, Memory, Agent } from "aircher";
import { codingArchetype, codingTools } from "aircher/archetypes/code";

const memory = new Memory({
  projectPath: process.cwd(),
  archetype: codingArchetype,
});

const agent = new Agent({
  memory,
  tools: codingTools,
  model: "deepseek/deepseek-v3.2-speciale",
});

const server = createACPServer({
  agent,
  capabilities: {
    streaming: true,
    tools: true,
  },
});

// Start server (stdio)
server.listen();
```

### ACP Server Interface

```typescript
interface ACPServer {
  listen(): void;
  close(): Promise<void>;
}

interface ACPServerOptions {
  agent: Agent;
  capabilities?: {
    streaming?: boolean;
    tools?: boolean;
    loadSession?: boolean;
  };
  onError?: (error: Error) => void;
}
```

## Error Handling

```typescript
import { AircherError, MemoryError, ContextError } from "aircher";

try {
  await memory.record({ type: "invalid", content: {} });
} catch (error) {
  if (error instanceof MemoryError) {
    console.error("Memory operation failed:", error.message);
  } else if (error instanceof ContextError) {
    console.error("Context assembly failed:", error.message);
  } else if (error instanceof AircherError) {
    console.error("Aircher error:", error.message);
  }
}
```

### Error Types

```typescript
class AircherError extends Error {
  code: string;
  cause?: Error;
}

class MemoryError extends AircherError {
  code: "MEMORY_INIT" | "MEMORY_RECORD" | "MEMORY_QUERY" | "MEMORY_PERSIST";
}

class ContextError extends AircherError {
  code: "CONTEXT_BUILD" | "CONTEXT_COMPRESS" | "CONTEXT_TOKEN_LIMIT";
}

class AgentError extends AircherError {
  code: "AGENT_RUN" | "AGENT_TOOL" | "AGENT_MODEL";
}
```

## TypeScript Types

All types are exported:

```typescript
import type {
  // Core
  Memory,
  Agent,
  Archetype,
  Tool,

  // Events
  Event,
  EventInput,

  // Entities
  Entity,
  Relation,
  Pattern,

  // Context
  Context,
  ContextRequest,
  ContextStrategy,
  ContextComponent,

  // Stores
  EpisodicStore,
  SemanticStore,
  ProceduralStore,
  RelationalStore,
  VectorStore,

  // Configuration
  AircherConfig,
  MemoryConfig,
  AgentConfig,
} from "aircher";
```

## Examples

### Basic Usage

```typescript
import { Memory } from "aircher";

async function main() {
  const memory = new Memory({ projectPath: "." });

  // Record some events
  await memory.record({
    type: "file_read",
    content: { path: "src/auth.ts", text: "..." },
  });

  await memory.record({
    type: "error",
    content: { message: "Token expired", stack: "..." },
    outcome: "failure",
  });

  // Build context for a prompt
  const context = await memory.recall({
    query: "fix the token expiration bug",
  });

  console.log(context.content);
  // Output: Assembled context with recent activity, relevant code, etc.

  await memory.close();
}
```

### With Agent

```typescript
import { Agent, Memory } from "aircher";
import { codingTools } from "aircher/archetypes/code";

async function main() {
  const memory = new Memory({ projectPath: "." });

  const agent = new Agent({
    memory,
    tools: codingTools,
    model: "deepseek/deepseek-v3.2-speciale",
  });

  const result = await agent.run("fix the token expiration bug");

  console.log(result.response);
  console.log(`Completed in ${result.steps.length} steps`);
  console.log(`Cost: $${result.usage.totalCost.toFixed(4)}`);

  await agent.close();
}
```

### Custom Archetype

```typescript
import { Memory, createArchetype, Agent } from "aircher";

const supportArchetype = createArchetype({
  name: "support",
  description: "Customer support agent",
  eventTypes: ["message", "ticket_resolved"],
  entityTypes: ["customer", "ticket"],
  relationTypes: ["reported_by"],
  patternTypes: ["resolution"],
  extractors: {
    message: (event) => [
      {
        type: "customer",
        name: event.content.customerId,
      },
    ],
  },
  contextStrategy: {
    recencyWeight: 0.4,
    relevanceWeight: 0.4,
    salientWeight: 0.2,
    depthLimit: 1,
    includeFailures: false,
    budgetAllocation: { recent: 30, semantic: 35, entities: 20, relations: 15 },
  },
  tools: [
    // Custom support tools
  ],
});

const memory = new Memory({
  projectPath: ".",
  archetype: supportArchetype,
});

const agent = new Agent({
  memory,
  tools: supportArchetype.tools,
  model: "deepseek/deepseek-v3.2-speciale",
});
```

## References

- [memory-architecture.md](memory-architecture.md) - Storage details
- [archetypes.md](archetypes.md) - Archetype system
- [context-assembly.md](context-assembly.md) - Context building
