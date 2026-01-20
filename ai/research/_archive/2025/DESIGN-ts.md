# Aircher System Design

## Overview

**aircher** is a local-first memory framework for AI agents.

The framework provides memory primitives that work for any agent. Archetypes customize the framework for specific domains. The coding archetype is our primary focus, but the framework is general-purpose.

```
┌─────────────────────────────────────────────────────────────────────┐
│                           Agent                                      │
│                  (any domain: coding, research, support, etc.)       │
└─────────────────────────────────────────────────────────────────────┘
                                   │
┌─────────────────────────────────────────────────────────────────────┐
│                      Archetype Layer                                 │
│         (domain-specific: extractors, tools, context strategy)       │
└─────────────────────────────────────────────────────────────────────┘
                                   │
┌─────────────────────────────────────────────────────────────────────┐
│                      Memory Framework                                │
│                        (open source)                                 │
│                                                                      │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐ ┌───────────────┐  │
│  │  Episodic   │ │  Semantic   │ │ Relational  │ │  Contextual   │  │
│  │  (events)   │ │ (entities)  │ │  (graph)    │ │  (vectors)    │  │
│  │   SQLite    │ │   SQLite    │ │ Graphology  │ │    OmenDB     │  │
│  └─────────────┘ └─────────────┘ └─────────────┘ └───────────────┘  │
│                                                                      │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │                     Context Assembly                             ││
│  │           (builds optimal prompts from memory)                   ││
│  └─────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────┘
```

## Core Concepts

### Memory vs Context

| Term    | Definition                  | Persistence     |
| ------- | --------------------------- | --------------- |
| Memory  | Everything we know/remember | Durable storage |
| Context | What we send to the LLM     | Per-request     |

**Key insight:** Store everything, send only what's relevant.

### Framework vs Archetype

| Layer         | Provides                      | Examples                           |
| ------------- | ----------------------------- | ---------------------------------- |
| **Framework** | Storage, indexing, retrieval  | EpisodicStore, VectorStore         |
| **Archetype** | Domain-specific customization | CodingArchetype, ResearchArchetype |

Framework is stable. Archetypes evolve.

## Memory Types

### Core (v1)

| Type       | Purpose             | Storage    | Key Features                  |
| ---------- | ------------------- | ---------- | ----------------------------- |
| Episodic   | What happened       | SQLite     | Causal chains, outcomes       |
| Semantic   | What we know        | SQLite     | Entities, salience scoring    |
| Relational | How things connect  | Graphology | Graph traversal, paths        |
| Contextual | Semantic similarity | OmenDB     | Vector search, hybrid queries |

### Future (v2)

| Type        | Purpose           | Storage |
| ----------- | ----------------- | ------- |
| Procedural  | What works        | SQLite  |
| User        | Who we're helping | SQLite  |
| Prospective | What to do        | SQLite  |
| Meta        | What we can do    | SQLite  |

## Data Models

### Event (Episodic)

```typescript
interface Event {
  id: string;
  sessionId: string;
  timestamp: number;

  type: string; // Archetype-defined
  actor: "user" | "agent" | "system";
  content: unknown;

  outcome?: "success" | "failure" | "partial";
  parentId?: string; // Causal chain
  duration?: number;

  summary?: string;
  keywords: string[];
}
```

### Entity (Semantic)

```typescript
interface Entity {
  id: string;
  type: string; // Archetype-defined (file, person, concept)
  name: string;
  aliases: string[];

  properties: Record<string, unknown>;
  description?: string;

  firstSeen: number;
  lastSeen: number;
  mentionCount: number;
  salience: number; // 0-1, importance
}
```

### Relation (Relational)

```typescript
interface Relation {
  id: string;
  type: string; // Archetype-defined (imports, knows, causes)

  sourceId: string;
  targetId: string;
  weight: number;

  evidence: string[]; // Event IDs supporting this
}
```

## Archetype Pattern

Archetypes customize the framework for specific domains.

```typescript
interface Archetype {
  name: string;
  description: string;

  // Domain-specific types
  eventTypes: string[];
  entityTypes: string[];
  relationTypes: string[];

  // Extractors: derive knowledge from events
  extractors: {
    [eventType: string]: (event: Event) => Entity[];
  };

  relationExtractors: {
    [eventType: string]: (event: Event, entities: Entity[]) => Relation[];
  };

  // Context strategy
  contextStrategy: {
    recencyWeight: number; // 0-1
    relevanceWeight: number; // 0-1
    salientWeight: number; // 0-1
    budgetAllocation: {
      recent: number; // % of tokens
      semantic: number;
      entities: number;
      relations: number;
    };
  };

  // Domain tools
  tools: ToolDefinition[];
}
```

### Coding Archetype (Primary)

```typescript
const codingArchetype: Archetype = {
  name: "coding",
  description: "AI coding agent for software development",

  eventTypes: [
    "file_read",
    "file_write",
    "file_edit",
    "shell_exec",
    "search",
    "error",
    "user_message",
    "agent_response",
  ],

  entityTypes: [
    "file",
    "function",
    "class",
    "module",
    "variable",
    "type",
    "error_pattern",
  ],

  relationTypes: [
    "imports",
    "exports",
    "calls",
    "extends",
    "implements",
    "uses",
    "tests",
    "caused_by",
  ],

  extractors: {
    file_read: (event) => {
      const entities = extractCodeEntities(event.content.text);
      return entities.map((e) => ({
        type: e.type,
        name: e.name,
        properties: {
          path: event.content.path,
          language: detectLanguage(event.content.path),
          ...e.properties,
        },
      }));
    },
    // ... other extractors
  },

  contextStrategy: {
    recencyWeight: 0.3,
    relevanceWeight: 0.5,
    salientWeight: 0.2,
    budgetAllocation: {
      recent: 15,
      semantic: 40,
      entities: 25,
      relations: 20,
    },
  },

  tools: [
    { name: "read_file", description: "Read file contents" },
    { name: "write_file", description: "Write file contents" },
    { name: "edit_file", description: "Edit with search/replace" },
    { name: "bash", description: "Execute shell command" },
    { name: "glob", description: "Find files by pattern" },
    { name: "grep", description: "Search file contents" },
  ],
};
```

### Other Archetypes (Future/Community)

```typescript
// Research archetype
const researchArchetype: Archetype = {
  name: "research",
  eventTypes: ["search", "read_source", "extract_fact", "cite"],
  entityTypes: ["source", "author", "claim", "topic"],
  relationTypes: ["cites", "supports", "contradicts", "authored_by"],
  // ...
};

// Support archetype
const supportArchetype: Archetype = {
  name: "support",
  eventTypes: ["message", "ticket_created", "ticket_resolved"],
  entityTypes: ["customer", "ticket", "product", "issue"],
  relationTypes: ["reported_by", "resolved_by", "related_to"],
  // ...
};
```

## Context Assembly

The key innovation: building optimal prompts from memory.

```typescript
interface ContextBuilder {
  build(request: ContextRequest): Promise<Context>;
}

interface ContextRequest {
  query: string;
  sessionId: string;
  maxTokens: number;
  strategy?: ContextStrategy;
}
```

### Algorithm

```typescript
async function buildContext(req: ContextRequest): Promise<Context> {
  const budget = new TokenBudget(req.maxTokens);
  const strategy = req.strategy || archetype.contextStrategy;

  // 1. Recent events (recency)
  const recent = await episodic.getRecent(req.sessionId, {
    limit: 20,
    tokens: budget.allocate(strategy.budgetAllocation.recent / 100),
  });

  // 2. Semantic search (relevance)
  const relevant = await contextual.search(req.query, {
    k: 20,
    tokens: budget.allocate(strategy.budgetAllocation.semantic / 100),
  });

  // 3. Related entities (salience)
  const entities = await semantic.findRelated(req.query, {
    tokens: budget.allocate(strategy.budgetAllocation.entities / 100),
  });

  // 4. Graph expansion
  const expanded = await relational.expand(entities, {
    depth: 2,
    tokens: budget.allocate(strategy.budgetAllocation.relations / 100),
  });

  // Score and rank
  const ranked = rankResults(
    [...recent, ...relevant, ...entities, ...expanded],
    strategy,
  );

  return formatContext(ranked, budget.remaining);
}
```

### Scoring

```typescript
function scoreResult(result: MemoryResult, weights: ContextStrategy): number {
  let score = 0;

  // Recency: exponential decay
  const ageHours = (Date.now() - result.timestamp) / (1000 * 60 * 60);
  score += Math.exp(-ageHours / 24) * weights.recencyWeight;

  // Relevance: semantic similarity
  if (result.similarity) {
    score += result.similarity * weights.relevanceWeight;
  }

  // Salience: entity importance
  if (result.salience) {
    score += result.salience * weights.salientWeight;
  }

  // Outcome boost
  if (result.outcome === "success") score *= 1.2;
  if (result.outcome === "failure") score *= 0.8;

  return score;
}
```

## API

### Basic Usage

```typescript
import { Memory } from "aircher";

// General-purpose (no archetype)
const memory = new Memory({ path: ".aircher" });

await memory.record({
  type: "action",
  content: { action: "searched", query: "how to..." },
});

const context = await memory.recall({
  query: "user question",
  maxTokens: 8000,
});
```

### With Archetype

```typescript
import { Memory } from "aircher";
import { codingArchetype } from "aircher/archetypes/coding";

const memory = new Memory({
  path: ".aircher",
  archetype: codingArchetype,
});

// Archetype extractors automatically run
await memory.record({
  type: "file_read",
  content: { path: "src/auth.ts", text: "..." },
});
// Extracts: functions, classes, imports, etc.
```

### Agent Integration

```typescript
import { generateText } from "ai";

async function run(prompt: string): Promise<string> {
  const context = await memory.recall({ query: prompt, maxTokens: 8000 });

  const result = await generateText({
    model,
    system: context.toSystemPrompt(),
    prompt,
    tools: archetype.tools,
    maxSteps: 20,
    onStepFinish: async (step) => {
      await memory.record({
        type: step.toolName,
        content: step.toolResult,
        outcome: step.success ? "success" : "failure",
      });
    },
  });

  return result.text;
}
```

## Storage

```
~/.aircher/
├── data/
│   └── projects/
│       └── {project-hash}/
│           ├── memory.db     # SQLite (events, entities, relations)
│           └── vectors/      # OmenDB
└── config.json

<project>/.aircher/           # Optional project-local
├── context.md                # Human-readable context
└── patterns.md               # Learned patterns
```

## Differentiators

| Feature       | Letta        | Mem0        | aircher            |
| ------------- | ------------ | ----------- | ------------------ |
| Architecture  | Server-based | Cloud-first | **Single binary**  |
| Deployment    | Run server   | API calls   | **Import library** |
| Privacy       | Self-host OK | Cloud       | **100% local**     |
| Complexity    | High         | Medium      | **Low**            |
| Customization | Moderate     | Limited     | **Archetypes**     |
| Target        | Enterprise   | Startups    | **Developers**     |

## References

- [ROADMAP.md](ROADMAP.md) - Implementation phases
- [DECISIONS.md](DECISIONS.md) - Decision records
- [design/memory-architecture.md](design/memory-architecture.md) - Full memory spec
- [design/archetypes.md](design/archetypes.md) - Archetype details
- [design/context-assembly.md](design/context-assembly.md) - Context building
- [design/api-design.md](design/api-design.md) - Public API
