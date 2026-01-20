# Phase 8 Implementation Plan (Revised)

**Purpose**: Minimal viable coding agent, then iterate

## Design Principles

1. **Prove value before building infrastructure** - Don't build Graph memory until Agent works
2. **One interface for MVP** - ACP server (works with Toad, Zed, Neovim)
3. **Simple before sophisticated** - Basic truncation, not tiered compaction
4. **Direct over wrapped** - AI SDK v5 directly, not through Mastra

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                     Entry Points                         │
│   aircher serve (ACP)  │  aircher --tui (future)        │
└───────────────┬─────────────────────────────────────────┘
                │
┌───────────────▼─────────────────────────────────────────┐
│                   AircherAgent                           │
│              (AI SDK v5 generateText)                    │
│   prepareStep → inject context from memory               │
│   maxSteps → multi-turn tool use                         │
└───────────────┬─────────────────────────────────────────┘
                │
┌───────────────▼─────────────────────────────────────────┐
│                      Tools                               │
│   read_file, write_file, edit_file, bash, glob, grep    │
└───────────────┬─────────────────────────────────────────┘
                │
┌───────────────▼─────────────────────────────────────────┐
│                     Memory                               │
│   Episodic (libsql)  │  Vector (Orama)                  │
│   - events log       │  - semantic search               │
│   - tool history     │  - code retrieval                │
└─────────────────────────────────────────────────────────┘
```

## Dependency Graph (Corrected)

```
[1] Types & Config
       ↓
[2] Tools ────────────────┐
       ↓                  │
[3] Agent (AI SDK v5) ←───┘
       ↓
[4] Episodic Memory (libsql)
       ↓
[5] Vector Memory (Orama)
       ↓
[6] ACP Server
       ↓
[7] Distribution

--- POST-MVP ---
[8] TUI (OpenTUI)
[9] Graph Memory (Graphology + tree-sitter)
[10] Advanced Compaction
```

## MVP Implementation

### 1. Types & Config

**Files**: `src/types.ts`, `src/config.ts`
**Effort**: Small

```typescript
// src/types.ts
export type AgentMode = "read" | "write" | "admin";

export interface AgentEvent {
  id: string;
  timestamp: number;
  type: "user" | "assistant" | "tool_call" | "tool_result";
  content: unknown;
}

export interface ToolResult {
  success: boolean;
  output: string;
  error?: string;
}

// src/config.ts
export interface Config {
  model: string; // Default: deepseek/deepseek-v3.2-speciale
  baseUrl: string; // Default: https://openrouter.ai/api/v1
  apiKey: string; // From env
  mode: AgentMode; // Default: write
  dataDir: string; // Default: ~/.aircher
  projectDir?: string; // Auto-detected from .git
}

export function loadConfig(): Config;
export function getProjectHash(path: string): string;
```

**Validation**: Config loads from env, types compile

---

### 2. Tools

**Files**: `src/tools/index.ts`, `src/tools/*.ts`
**Effort**: Medium

```typescript
// Using AI SDK tool format
import { tool } from "ai";
import { z } from "zod";

export const readFile = tool({
  description: "Read file contents",
  parameters: z.object({
    path: z.string(),
    startLine: z.number().optional(),
    endLine: z.number().optional(),
  }),
  execute: async ({ path, startLine, endLine }) => {
    /* ... */
  },
});

// 6 core tools:
export const tools = {
  read_file: readFile,
  write_file: writeFile,
  edit_file: editFile, // find/replace
  bash: bashTool,
  glob: globTool,
  grep: grepTool,
};
```

**Validation**: Each tool works standalone

---

### 3. Agent (AI SDK v5)

**Files**: `src/agent/index.ts`
**Effort**: Medium

```typescript
import { generateText } from "ai";
import { createOpenAI } from "@ai-sdk/openai";
import { tools } from "../tools";

export class Agent {
  private config: Config;
  private memory: Memory | null = null;

  constructor(config: Config) {
    this.config = config;
  }

  async run(prompt: string): Promise<string> {
    const openai = createOpenAI({
      baseURL: this.config.baseUrl,
      apiKey: this.config.apiKey,
    });

    const result = await generateText({
      model: openai(this.config.model),
      tools,
      maxSteps: 20,
      system: this.getSystemPrompt(),
      prompt,
      // Inject memory context before each step
      prepareStep: async ({ previousSteps }) => {
        const context = await this.memory?.getRelevant(prompt);
        return {
          messages: context ? [{ role: "system", content: context }] : [],
        };
      },
    });

    // Record to memory
    await this.memory?.record({
      type: "assistant",
      content: result.text,
      timestamp: Date.now(),
    });

    return result.text;
  }

  setMemory(memory: Memory) {
    this.memory = memory;
  }
}
```

**Validation**: Agent responds, uses tools, handles errors

---

### 4. Episodic Memory (bun:sqlite)

**Files**: `src/memory/episodic.ts`
**Effort**: Medium

```typescript
import { Database } from "bun:sqlite";

export class EpisodicMemory {
  private db: Database;

  constructor(dbPath: string) {
    this.db = new Database(dbPath);
    this.db.run("PRAGMA journal_mode = WAL;");
    this.db.run(`
      CREATE TABLE IF NOT EXISTS events (
        id TEXT PRIMARY KEY,
        timestamp INTEGER NOT NULL,
        type TEXT NOT NULL,
        content TEXT NOT NULL
      )
    `);
  }

  record(event: AgentEvent): void {
    const stmt = this.db.prepare(
      "INSERT INTO events (id, timestamp, type, content) VALUES (?, ?, ?, ?)",
    );
    stmt.run(
      crypto.randomUUID(),
      event.timestamp,
      event.type,
      JSON.stringify(event.content),
    );
  }

  getRecent(limit = 50): AgentEvent[] {
    const stmt = this.db.prepare(
      "SELECT * FROM events ORDER BY timestamp DESC LIMIT ?",
    );
    return stmt.all(limit).map((row: any) => ({
      id: row.id,
      timestamp: row.timestamp,
      type: row.type,
      content: JSON.parse(row.content),
    }));
  }
}
```

**Why bun:sqlite**: Built-in (zero deps), 3-6x faster, synchronous API perfect for CLI.

**Validation**: Events persist across restarts

---

### 5. Vector Memory (OmenDB)

**Files**: `src/memory/vector.ts`, `src/memory/vector-store.ts`
**Effort**: Medium

```typescript
// src/memory/vector-store.ts - Abstract interface (allows Orama fallback)
export interface VectorStore {
  index(path: string, content: string, embedding: number[]): Promise<void>;
  search(embedding: number[], k?: number): Promise<SearchResult[]>;
  persist(path: string): Promise<void>;
  restore(path: string): Promise<void>;
}

export interface SearchResult {
  path: string;
  content: string;
  score: number;
}

// src/memory/vector.ts - OmenDB implementation
import { OmenDB } from "omendb";

export class OmenDBVectorStore implements VectorStore {
  private db: OmenDB;

  constructor(dimension: number = 384) {
    this.db = new OmenDB({ dimension });
  }

  async index(
    path: string,
    content: string,
    embedding: number[],
  ): Promise<void> {
    await this.db.insert(embedding, { path, content });
  }

  async search(embedding: number[], k = 10): Promise<SearchResult[]> {
    const results = await this.db.search(embedding, k);
    return results.map((r) => ({
      path: r.metadata.path,
      content: r.metadata.content,
      score: r.distance,
    }));
  }

  async persist(path: string): Promise<void> {
    await this.db.save(path);
  }

  async restore(path: string): Promise<void> {
    await this.db.load(path);
  }
}

// Fallback: OramaVectorStore implements VectorStore (if OmenDB blocks progress)
```

**Why OmenDB**: Dogfooding to find/fix bugs in real-world use.
**Fallback**: Orama available if OmenDB blocks MVP progress.

````

**Validation**: Semantic search returns relevant results

---

### 6. ACP Server

**Files**: `src/protocol/acp.ts`
**Effort**: Medium

```typescript
import { ACPServer } from "@agentclientprotocol/sdk";

export function createServer(agent: Agent): ACPServer {
  return new ACPServer({
    name: "aircher",
    version: "0.1.0",
    handlers: {
      async runTask({ prompt }) {
        return agent.run(prompt);
      },
      async listTools() {
        return Object.keys(tools);
      },
    },
  });
}

// Entry point: aircher serve
export async function serve() {
  const config = loadConfig();
  const agent = new Agent(config);
  const memory = await EpisodicMemory.create(`${config.dataDir}/events.db`);
  agent.setMemory(memory);

  const server = createServer(agent);
  await server.listen(); // stdio transport
}
````

**Validation**: Toad connects and runs tasks

---

### 7. Distribution

```bash
bun build ./src/index.ts --compile --outfile aircher
```

**Validation**: Binary runs, under 50MB, startup under 50ms

---

## Post-MVP (Deferred)

| Feature                 | Why Deferred                      | Revisit When           |
| ----------------------- | --------------------------------- | ---------------------- |
| **TUI (OpenTUI)**       | ACP server covers most use cases  | User demand            |
| **Graph Memory**        | Unproven value for coding agent   | After benchmarking     |
| **Self-edit tools**     | Auto-record sufficient for MVP    | Memory grows too large |
| **Advanced compaction** | Simple truncation works initially | Context limit issues   |
| **tree-sitter**         | Needed for graph, graph deferred  | With graph memory      |

---

## Implementation Order

| #   | Task            | Depends On | Effort | Cumulative |
| --- | --------------- | ---------- | ------ | ---------- |
| 1   | Types & Config  | -          | S      | S          |
| 2   | Tools           | 1          | M      | S+M        |
| 3   | Agent           | 1, 2       | M      | S+2M       |
| 4   | Episodic Memory | 1          | M      | S+3M       |
| 5   | Vector Memory   | 1          | M      | S+4M       |
| 6   | ACP Server      | 3          | M      | S+5M       |
| 7   | Distribution    | All        | S      | 2S+5M      |

**Total**: 2 Small + 5 Medium tasks

---

## Dependencies

```json
{
  "dependencies": {
    "ai": "^4.0",
    "@ai-sdk/openai": "^1.0",
    "omendb": "^0.0.11",
    "@agentclientprotocol/sdk": "^0.11",
    "zod": "^3.23"
  }
}
```

**Built-in** (no package needed): `bun:sqlite`

**Removed**: `@mastra/core`, `@libsql/client`, `graphology`, `web-tree-sitter`, `@ast-grep/napi`, `@opentui/*`

**Fallback** (if OmenDB blocks): `@orama/orama`, `@orama/plugin-data-persistence`

---

## Success Criteria

- [ ] Agent answers coding questions
- [ ] Agent reads/writes files correctly
- [ ] Agent executes shell commands
- [ ] Memory persists across restarts
- [ ] Toad connects via ACP
- [ ] Binary under 50MB
- [ ] Startup under 50ms

---

## What Changed from Previous Plan

| Before             | After                        | Why                           |
| ------------------ | ---------------------------- | ----------------------------- |
| Mastra framework   | AI SDK v5 direct             | Mastra adds unnecessary layer |
| 3 memory layers    | 2 layers (episodic + vector) | Graph unproven                |
| Graph before Agent | Graph deferred               | Prove agent works first       |
| Vector after Agent | Vector with Agent            | Needed for retrieval          |
| ACP + TUI          | ACP only                     | One interface for MVP         |
| Self-edit tools    | Auto-record only             | Simpler for MVP               |
| Tiered compaction  | Simple truncation            | Defer complexity              |
