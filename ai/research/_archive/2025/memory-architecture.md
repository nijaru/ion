# Memory Architecture Specification

## Overview

This document specifies the comprehensive memory system for aircher - a 10-type memory model that covers all agent memory needs.

## Design Principles

1. **Cognitive-inspired** - Based on human memory systems
2. **Storage-efficient** - Single SQLite database for most types
3. **Query-optimized** - Fast retrieval for context assembly
4. **Domain-agnostic** - Archetypes customize, framework stores

## Memory Types

### 1. Working Memory

**Purpose:** Current focus, active task, scratchpad

**Storage:** In-memory only (not persisted)

**Scope:** Single request/turn

```typescript
interface WorkingMemory {
  sessionId: string;
  currentGoal: string | null;
  activeTask: Task | null;
  recentOutputs: ToolOutput[];
  scratchpad: Map<string, unknown>;

  // Token management
  contextWindow: number;
  usedTokens: number;
}
```

**Operations:**

- `set(key, value)` - Store temporary data
- `get(key)` - Retrieve temporary data
- `clear()` - Reset working memory

### 2. Episodic Memory

**Purpose:** What happened (events, actions, outcomes)

**Storage:** SQLite `events` table

**Scope:** Cross-session

```typescript
interface Event {
  id: string;
  sessionId: string;
  timestamp: number;

  type: string; // Archetype-defined
  actor: "user" | "agent" | "system";
  content: unknown;

  outcome?: "success" | "failure" | "partial";
  parentId?: string; // Causality chain
  duration?: number; // Execution time

  // Retrieval
  summary?: string;
  keywords: string[];
}
```

**Schema:**

```sql
CREATE TABLE events (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  timestamp INTEGER NOT NULL,
  type TEXT NOT NULL,
  actor TEXT NOT NULL,
  content JSON,
  outcome TEXT,
  parent_id TEXT,
  duration INTEGER,
  summary TEXT,
  keywords JSON,

  FOREIGN KEY (parent_id) REFERENCES events(id)
);

CREATE INDEX idx_events_session ON events(session_id, timestamp DESC);
CREATE INDEX idx_events_type ON events(type, timestamp DESC);
CREATE INDEX idx_events_outcome ON events(outcome) WHERE outcome IS NOT NULL;
```

**Operations:**

- `record(event)` - Append new event
- `getRecent(sessionId, limit)` - Recent events for session
- `getByType(type, limit)` - Events by type
- `getChain(eventId)` - Causal chain (parent/children)
- `search(query)` - Keyword search

### 3. Semantic Memory (Entities)

**Purpose:** What we know (things we've learned about)

**Storage:** SQLite `entities` table

**Scope:** Cross-session

```typescript
interface Entity {
  id: string;
  type: string; // Archetype-defined (file, function, person)
  name: string;
  aliases: string[];

  properties: Record<string, unknown>;
  description?: string;

  firstSeen: number;
  lastSeen: number;
  mentionCount: number;
  salience: number; // 0-1, importance
  confidence: number; // 0-1, certainty
}
```

**Schema:**

```sql
CREATE TABLE entities (
  id TEXT PRIMARY KEY,
  type TEXT NOT NULL,
  name TEXT NOT NULL,
  aliases JSON DEFAULT '[]',
  properties JSON DEFAULT '{}',
  description TEXT,
  first_seen INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  mention_count INTEGER DEFAULT 1,
  salience REAL DEFAULT 0.5,
  confidence REAL DEFAULT 0.5
);

CREATE INDEX idx_entities_type ON entities(type);
CREATE INDEX idx_entities_name ON entities(name);
CREATE INDEX idx_entities_salience ON entities(salience DESC);
```

**Operations:**

- `upsert(entity)` - Create or update entity
- `get(id)` - Get entity by ID
- `findByName(name)` - Find by name/alias
- `getByType(type, limit)` - Get by type
- `getSalient(limit)` - Most important entities

### 4. Procedural Memory (Patterns)

**Purpose:** What works (learned patterns, solutions)

**Storage:** SQLite `patterns` table

**Scope:** Cross-session (can be global or project-scoped)

```typescript
interface Pattern {
  id: string;
  type: string; // solution, workflow, anti-pattern
  name: string;
  description: string;

  trigger: {
    conditions: Condition[];
    confidence: number;
  };

  procedure: {
    steps: Step[];
    tools: string[];
    expectedOutcome: string;
  };

  // Track record
  applications: number;
  successes: number;
  failures: number;
  lastUsed: number;

  // Provenance
  sourceEvents: string[];
  scope: "global" | "project";
}

interface Condition {
  type: "keyword" | "entity_type" | "error_type" | "custom";
  value: string;
  weight: number;
}

interface Step {
  action: string;
  tool?: string;
  parameters?: Record<string, unknown>;
}
```

**Schema:**

```sql
CREATE TABLE patterns (
  id TEXT PRIMARY KEY,
  type TEXT NOT NULL,
  name TEXT NOT NULL,
  description TEXT,
  trigger JSON NOT NULL,
  procedure JSON NOT NULL,
  applications INTEGER DEFAULT 0,
  successes INTEGER DEFAULT 0,
  failures INTEGER DEFAULT 0,
  last_used INTEGER,
  source_events JSON DEFAULT '[]',
  scope TEXT DEFAULT 'project'
);

CREATE INDEX idx_patterns_type ON patterns(type);
CREATE INDEX idx_patterns_success_rate ON patterns(
  CAST(successes AS REAL) / NULLIF(applications, 0) DESC
);
```

**Operations:**

- `record(pattern)` - Store new pattern
- `match(context)` - Find matching patterns
- `applyResult(patternId, success)` - Update track record
- `getBest(type, limit)` - Best patterns by type

### 5. Relational Memory (Graph)

**Purpose:** How things connect

**Storage:** SQLite `relations` table + in-memory Graphology

**Scope:** Cross-session

```typescript
interface Relation {
  id: string;
  type: string; // Archetype-defined (imports, calls, caused)

  sourceId: string; // Entity or Event ID
  sourceType: "entity" | "event";
  targetId: string;
  targetType: "entity" | "event";

  weight: number; // 0-1
  confidence: number;

  evidence: string[]; // Event IDs that support this
  metadata: Record<string, unknown>;
}
```

**Schema:**

```sql
CREATE TABLE relations (
  id TEXT PRIMARY KEY,
  type TEXT NOT NULL,
  source_id TEXT NOT NULL,
  source_type TEXT NOT NULL,
  target_id TEXT NOT NULL,
  target_type TEXT NOT NULL,
  weight REAL DEFAULT 1.0,
  confidence REAL DEFAULT 0.5,
  evidence JSON DEFAULT '[]',
  metadata JSON DEFAULT '{}'
);

CREATE INDEX idx_relations_source ON relations(source_id);
CREATE INDEX idx_relations_target ON relations(target_id);
CREATE INDEX idx_relations_type ON relations(type);
```

**In-Memory Graph (Graphology):**

```typescript
import Graph from "graphology";

// Build graph from relations table
const graph = new Graph();

// Add nodes (entities)
for (const entity of entities) {
  graph.addNode(entity.id, entity);
}

// Add edges (relations)
for (const relation of relations) {
  graph.addEdge(relation.sourceId, relation.targetId, relation);
}
```

**Operations:**

- `addRelation(relation)` - Add new relation
- `getRelations(entityId)` - Relations for entity
- `traverse(entityId, depth)` - BFS traversal
- `findPath(sourceId, targetId)` - Shortest path
- `getNeighbors(entityId, relationTypes)` - Filtered neighbors

### 6. Contextual Memory (Vectors)

**Purpose:** Semantic similarity search

**Storage:** OmenDB (external file)

**Scope:** Cross-session

```typescript
interface ContentChunk {
  id: string;
  sourceId: string; // Entity or Event this came from
  sourceType: "entity" | "event" | "content";

  text: string;
  embedding: number[];
  chunkIndex: number;

  metadata: Record<string, unknown>;
  timestamp: number;
}
```

**Operations:**

```typescript
interface VectorStore {
  // Index content
  index(chunk: ContentChunk): Promise<void>;
  indexBatch(chunks: ContentChunk[]): Promise<void>;

  // Search
  search(query: string, k?: number): Promise<SearchResult[]>;
  searchByVector(embedding: number[], k?: number): Promise<SearchResult[]>;

  // Hybrid search
  hybridSearch(query: string, filters?: Filter[]): Promise<SearchResult[]>;

  // Maintenance
  delete(id: string): Promise<void>;
  persist(): Promise<void>;
  restore(): Promise<void>;
}

interface SearchResult {
  chunk: ContentChunk;
  score: number;
  distance: number;
}
```

**Embedding Strategy:**

- Local: Ollama embeddings (privacy, speed)
- API: OpenAI text-embedding-3-small (quality)
- Chunking: By content type (functions, paragraphs, messages)

### 7. User Memory

**Purpose:** Who we're helping (preferences, history)

**Storage:** SQLite `user_profiles` table

**Scope:** Per-user

```typescript
interface UserProfile {
  id: string;

  // Preferences
  communicationStyle: "concise" | "detailed" | "balanced";
  expertiseLevel: "beginner" | "intermediate" | "expert";
  preferredTools: string[];

  // Learned from interaction
  corrections: Correction[];
  feedback: Feedback[];

  // Custom instructions
  customInstructions: string[];

  // Stats
  totalSessions: number;
  firstSeen: number;
  lastSeen: number;
}

interface Correction {
  timestamp: number;
  original: string;
  corrected: string;
  context: string;
}
```

### 8. Prospective Memory (Goals)

**Purpose:** What to do (intentions, plans)

**Storage:** SQLite `goals` table

**Scope:** Session or persistent

```typescript
interface Goal {
  id: string;
  description: string;
  status: "pending" | "active" | "blocked" | "completed" | "abandoned";

  // Decomposition
  parentGoalId?: string;
  subgoals: string[];

  // Progress
  steps: Step[];
  currentStep: number;
  completedSteps: number[];

  // Context
  sessionId?: string; // null = persistent goal
  createdAt: number;
  deadline?: number;
  priority: number;

  // Blocking
  blockedBy?: string[];
  blockedReason?: string;
}
```

### 9. Meta Memory (Self-Knowledge)

**Purpose:** What we can do (capabilities, limitations)

**Storage:** SQLite `capabilities` table

**Scope:** Agent-wide

```typescript
interface Capabilities {
  // Available tools
  tools: ToolDefinition[];

  // Performance tracking
  toolStats: Record<string, ToolStats>;

  // Self-assessment
  strengths: string[];
  weaknesses: string[];

  // Boundaries
  cannotDo: string[];
  shouldAsk: string[];
}

interface ToolStats {
  tool: string;
  uses: number;
  successes: number;
  failures: number;
  avgDuration: number;
  commonErrors: string[];
}
```

### 10. Environmental Memory (World Model)

**Purpose:** Where we are (current state)

**Storage:** In-memory only

**Scope:** Session

```typescript
interface Environment {
  // File system
  projectRoot: string;
  fileTree: FileNode[];
  recentChanges: FileChange[];

  // Project understanding
  projectType: string;
  buildSystem: string;
  conventions: string[];

  // External state
  services: ServiceState[];
  gitStatus: GitStatus;

  // Freshness
  lastScanned: number;
  staleAfter: number;
}
```

## Storage Architecture

### Single SQLite Database

All relational memory types share one database:

```
~/.aircher/data/projects/{hash}/memory.db
├── events          (episodic)
├── entities        (semantic)
├── relations       (relational)
├── patterns        (procedural)
├── user_profiles   (user)
├── goals           (prospective)
└── capabilities    (meta)
```

### Vector Storage (Separate)

```
~/.aircher/data/projects/{hash}/vectors/
└── index.omen      (OmenDB)
```

### Memory Initialization

```typescript
async function initializeMemory(projectPath: string): Promise<Memory> {
  const projectHash = hashPath(projectPath);
  const dataDir = `~/.aircher/data/projects/${projectHash}`;

  // SQLite for relational storage
  const db = new Database(`${dataDir}/memory.db`);
  await runMigrations(db);

  // OmenDB for vectors
  const vectors = await OmenDB.open(`${dataDir}/vectors/index.omen`);

  // In-memory graph (built from relations)
  const graph = await buildGraph(db);

  return new Memory({
    db,
    vectors,
    graph,
    projectPath,
  });
}
```

## Memory Lifecycle

### Recording Events

```typescript
async function recordEvent(
  memory: Memory,
  event: Partial<Event>,
): Promise<Event> {
  // 1. Create event
  const fullEvent: Event = {
    id: generateId(),
    timestamp: Date.now(),
    sessionId: currentSession,
    keywords: extractKeywords(event.content),
    ...event,
  };

  // 2. Store in episodic
  await memory.episodic.record(fullEvent);

  // 3. Extract entities (archetype-specific)
  const entities = archetype.extractors[event.type]?.(fullEvent) ?? [];
  for (const entity of entities) {
    await memory.semantic.upsert(entity);
  }

  // 4. Extract relations
  const relations =
    archetype.relationExtractors[event.type]?.(fullEvent, entities) ?? [];
  for (const relation of relations) {
    await memory.relational.add(relation);
  }

  // 5. Index content for vector search
  if (event.content && typeof event.content === "string") {
    await memory.contextual.index({
      id: fullEvent.id,
      sourceId: fullEvent.id,
      sourceType: "event",
      text: event.content,
      embedding: await embed(event.content),
      chunkIndex: 0,
      metadata: { type: event.type },
      timestamp: fullEvent.timestamp,
    });
  }

  return fullEvent;
}
```

### Querying Memory

```typescript
async function queryMemory(
  memory: Memory,
  query: string,
  options: QueryOptions,
): Promise<MemoryResults> {
  // Parallel queries to different memory types
  const [recentEvents, similarContent, relatedEntities] = await Promise.all([
    memory.episodic.search(query, options.eventLimit),
    memory.contextual.search(query, options.vectorLimit),
    memory.semantic.findByName(query),
  ]);

  // Expand relations
  const entityIds = relatedEntities.map((e) => e.id);
  const relations = await memory.relational.getRelations(entityIds);

  // Find applicable patterns
  const patterns = await memory.procedural.match({
    query,
    entities: relatedEntities,
    events: recentEvents,
  });

  return {
    events: recentEvents,
    content: similarContent,
    entities: relatedEntities,
    relations,
    patterns,
  };
}
```

## Performance Considerations

### Indexing Strategy

| Table     | Indexes                           | Purpose             |
| --------- | --------------------------------- | ------------------- |
| events    | session+timestamp, type+timestamp | Fast recent queries |
| entities  | type, name, salience              | Entity lookup       |
| relations | source, target, type              | Graph traversal     |
| patterns  | type, success_rate                | Pattern matching    |

### Query Optimization

1. **Limit results** - Always use LIMIT
2. **Use indexes** - Query by indexed columns
3. **Batch operations** - Group inserts in transactions
4. **Cache graph** - Keep Graphology in memory
5. **Lazy loading** - Don't load vectors until needed

### Memory Limits

| Memory Type | Max Items        | Pruning Strategy      |
| ----------- | ---------------- | --------------------- |
| Events      | 100K per project | Archive old sessions  |
| Entities    | 10K per project  | Prune low-salience    |
| Relations   | 50K per project  | Prune low-weight      |
| Patterns    | 1K per project   | Prune low-success     |
| Vectors     | 100K chunks      | Prune old, low-access |

## References

- [DESIGN.md](../DESIGN.md) - Framework overview
- [archetypes.md](archetypes.md) - Archetype pattern
- [context-assembly.md](context-assembly.md) - Context building
- [api-design.md](api-design.md) - Public API
