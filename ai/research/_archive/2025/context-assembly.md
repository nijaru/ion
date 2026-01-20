# Context Assembly Specification

## Overview

Context assembly is the process of selecting and combining relevant memory into prompts for LLMs. This is the core value of the memory framework - turning stored knowledge into actionable context.

## Design Principles

1. **Token-aware** - Stay within budget
2. **Relevance-focused** - Quality over quantity
3. **Composable** - Combine multiple memory sources
4. **Configurable** - Strategy per archetype

## Context Builder Interface

```typescript
interface ContextBuilder {
  /**
   * Build context for a query
   */
  build(request: ContextRequest): Promise<Context>;

  /**
   * Estimate tokens for content
   */
  estimateTokens(content: string): number;

  /**
   * Compress content to fit budget
   */
  compress(content: string, maxTokens: number): Promise<string>;
}

interface ContextRequest {
  /** User query/prompt */
  query: string;

  /** Current session ID */
  sessionId: string;

  /** Maximum tokens for context */
  maxTokens: number;

  /** Strategy override */
  strategy?: Partial<ContextStrategy>;

  /** Additional context to include */
  additionalContext?: string;

  /** Filter options */
  filters?: ContextFilters;
}

interface ContextFilters {
  /** Only events after this timestamp */
  since?: number;

  /** Only these event types */
  eventTypes?: string[];

  /** Only these entity types */
  entityTypes?: string[];

  /** Exclude these IDs */
  exclude?: string[];
}

interface Context {
  /** Assembled context string */
  content: string;

  /** Components that make up the context */
  components: ContextComponent[];

  /** Token count */
  tokenCount: number;

  /** Was content truncated? */
  truncated: boolean;

  /** Strategy used */
  strategy: ContextStrategy;

  /** Metadata for debugging */
  metadata: {
    query: string;
    eventsConsidered: number;
    entitiesConsidered: number;
    chunksSearched: number;
    assemblyTimeMs: number;
  };
}

interface ContextComponent {
  type: "recent" | "semantic" | "entity" | "relation" | "pattern" | "custom";
  source: string; // ID of source item
  content: string;
  tokens: number;
  relevanceScore: number;
}
```

## Assembly Algorithm

### High-Level Flow

```
Query arrives
     │
     ▼
┌─────────────────────────────────────┐
│ 1. Parse query, extract signals     │
│    - Keywords, entities, intent     │
└─────────────────────────────────────┘
     │
     ▼
┌─────────────────────────────────────┐
│ 2. Query memory sources in parallel │
│    - Recent events                  │
│    - Semantic search                │
│    - Entity lookup                  │
│    - Relation traversal             │
│    - Pattern matching               │
└─────────────────────────────────────┘
     │
     ▼
┌─────────────────────────────────────┐
│ 3. Score and rank results           │
│    - Relevance scoring              │
│    - Recency weighting              │
│    - Salience weighting             │
└─────────────────────────────────────┘
     │
     ▼
┌─────────────────────────────────────┐
│ 4. Select items within budget       │
│    - Greedy selection               │
│    - Budget allocation              │
└─────────────────────────────────────┘
     │
     ▼
┌─────────────────────────────────────┐
│ 5. Format and assemble              │
│    - Structure context              │
│    - Add headers/separators         │
└─────────────────────────────────────┘
     │
     ▼
Context ready
```

### Detailed Implementation

```typescript
async function buildContext(
  memory: Memory,
  request: ContextRequest,
): Promise<Context> {
  const startTime = Date.now();
  const strategy = { ...memory.archetype.contextStrategy, ...request.strategy };
  const budget = new TokenBudget(request.maxTokens);

  // 1. Parse query for signals
  const signals = await parseQuerySignals(request.query);

  // 2. Query memory sources in parallel
  const [recent, semantic, entities, relations, patterns] = await Promise.all([
    queryRecent(
      memory,
      request,
      budget.allocate(strategy.budgetAllocation.recent),
    ),
    querySemantic(
      memory,
      request,
      signals,
      budget.allocate(strategy.budgetAllocation.semantic),
    ),
    queryEntities(
      memory,
      signals,
      budget.allocate(strategy.budgetAllocation.entities),
    ),
    queryRelations(
      memory,
      entities,
      strategy.depthLimit,
      budget.allocate(strategy.budgetAllocation.relations),
    ),
    queryPatterns(memory, signals),
  ]);

  // 3. Score and rank all results
  const scored = scoreResults(
    {
      recent,
      semantic,
      entities,
      relations,
      patterns,
    },
    signals,
    strategy,
  );

  // 4. Select items within budget
  const selected = selectWithinBudget(scored, budget.remaining());

  // 5. Format and assemble
  const content = formatContext(selected, request);

  return {
    content,
    components: selected,
    tokenCount: budget.used(),
    truncated: scored.length > selected.length,
    strategy,
    metadata: {
      query: request.query,
      eventsConsidered: recent.length,
      entitiesConsidered: entities.length,
      chunksSearched: semantic.length,
      assemblyTimeMs: Date.now() - startTime,
    },
  };
}
```

## Query Parsing

Extract signals from the user query:

```typescript
interface QuerySignals {
  /** Keywords extracted */
  keywords: string[];

  /** Entities mentioned */
  mentionedEntities: string[];

  /** Detected intent */
  intent: "question" | "action" | "fix" | "explain" | "other";

  /** Relevant event types */
  relevantEventTypes: string[];

  /** Embedding for semantic search */
  embedding: number[];
}

async function parseQuerySignals(query: string): Promise<QuerySignals> {
  // Extract keywords (simple approach)
  const keywords = extractKeywords(query);

  // Detect intent
  const intent = detectIntent(query);

  // Map intent to relevant event types
  const relevantEventTypes = getRelevantEventTypes(intent);

  // Get embedding for semantic search
  const embedding = await embed(query);

  // Find mentioned entities (by keyword matching)
  const mentionedEntities = await findMentionedEntities(keywords);

  return {
    keywords,
    mentionedEntities,
    intent,
    relevantEventTypes,
    embedding,
  };
}

function detectIntent(query: string): QuerySignals["intent"] {
  const lower = query.toLowerCase();

  if (
    lower.includes("fix") ||
    lower.includes("error") ||
    lower.includes("bug")
  ) {
    return "fix";
  }
  if (
    lower.includes("explain") ||
    lower.includes("how does") ||
    lower.includes("what is")
  ) {
    return "explain";
  }
  if (lower.includes("?")) {
    return "question";
  }
  return "action";
}
```

## Memory Queries

### Recent Events

```typescript
async function queryRecent(
  memory: Memory,
  request: ContextRequest,
  tokenBudget: number,
): Promise<ScoredEvent[]> {
  // Get recent events from current session
  const sessionEvents = await memory.episodic.getRecent(
    request.sessionId,
    50, // Get more than we need, then filter
  );

  // Also get recent events from other recent sessions (cross-session context)
  const recentSessions = await memory.episodic.getRecentSessions(5);
  const crossSessionEvents =
    await memory.episodic.getSessionHighlights(recentSessions);

  // Combine and dedupe
  const allEvents = dedupeById([...sessionEvents, ...crossSessionEvents]);

  // Score by recency
  const scored = allEvents.map((event) => ({
    ...event,
    score: calculateRecencyScore(event.timestamp),
    tokens: estimateTokens(formatEvent(event)),
  }));

  // Filter to budget
  return selectWithinTokens(scored, tokenBudget);
}

function calculateRecencyScore(timestamp: number): number {
  const ageMs = Date.now() - timestamp;
  const ageHours = ageMs / (1000 * 60 * 60);

  // Exponential decay: score halves every 24 hours
  return Math.exp(-ageHours / 24);
}
```

### Semantic Search

```typescript
async function querySemantic(
  memory: Memory,
  request: ContextRequest,
  signals: QuerySignals,
  tokenBudget: number,
): Promise<ScoredChunk[]> {
  // Search by embedding similarity
  const results = await memory.contextual.search(
    signals.embedding,
    20, // Top 20 results
  );

  // Score by similarity + other factors
  const scored = results.map((result) => ({
    ...result.chunk,
    score: result.score * calculateBoosts(result.chunk, signals),
    tokens: estimateTokens(result.chunk.text),
  }));

  return selectWithinTokens(scored, tokenBudget);
}

function calculateBoosts(chunk: ContentChunk, signals: QuerySignals): number {
  let boost = 1.0;

  // Boost if chunk contains keywords
  const keywordMatches = signals.keywords.filter((kw) =>
    chunk.text.toLowerCase().includes(kw.toLowerCase()),
  );
  boost *= 1 + keywordMatches.length * 0.1;

  // Boost recent content
  const ageHours = (Date.now() - chunk.timestamp) / (1000 * 60 * 60);
  if (ageHours < 24) boost *= 1.2;

  return boost;
}
```

### Entity Lookup

```typescript
async function queryEntities(
  memory: Memory,
  signals: QuerySignals,
  tokenBudget: number,
): Promise<ScoredEntity[]> {
  const entities: Entity[] = [];

  // Find directly mentioned entities
  for (const name of signals.mentionedEntities) {
    const found = await memory.semantic.findByName(name);
    if (found) entities.push(found);
  }

  // Get highly salient entities
  const salient = await memory.semantic.getSalient(10);
  entities.push(...salient);

  // Dedupe and score
  const unique = dedupeById(entities);
  const scored = unique.map((entity) => ({
    ...entity,
    score:
      entity.salience *
      (signals.mentionedEntities.includes(entity.name) ? 2 : 1),
    tokens: estimateTokens(formatEntity(entity)),
  }));

  return selectWithinTokens(scored, tokenBudget);
}
```

### Relation Traversal

```typescript
async function queryRelations(
  memory: Memory,
  entities: ScoredEntity[],
  depthLimit: number,
  tokenBudget: number,
): Promise<ScoredRelation[]> {
  const entityIds = entities.map((e) => e.id);
  const visited = new Set<string>(entityIds);
  const relations: Relation[] = [];

  // BFS traversal
  let frontier = entityIds;
  for (let depth = 0; depth < depthLimit && frontier.length > 0; depth++) {
    const nextFrontier: string[] = [];

    for (const id of frontier) {
      const entityRelations = await memory.relational.getRelations(id);

      for (const relation of entityRelations) {
        relations.push(relation);

        // Add connected entities to next frontier
        const otherId =
          relation.sourceId === id ? relation.targetId : relation.sourceId;
        if (!visited.has(otherId)) {
          visited.add(otherId);
          nextFrontier.push(otherId);
        }
      }
    }

    frontier = nextFrontier;
  }

  // Score by weight and depth
  const scored = relations.map((relation) => ({
    ...relation,
    score: relation.weight,
    tokens: estimateTokens(formatRelation(relation)),
  }));

  return selectWithinTokens(scored, tokenBudget);
}
```

### Pattern Matching

```typescript
async function queryPatterns(
  memory: Memory,
  signals: QuerySignals,
): Promise<Pattern[]> {
  // Find patterns that match current context
  const patterns = await memory.procedural.match({
    keywords: signals.keywords,
    intent: signals.intent,
    entityTypes: signals.mentionedEntities.map((e) => e.type),
  });

  // Sort by success rate
  return patterns.sort(
    (a, b) => b.successes / b.applications - a.successes / a.applications,
  );
}
```

## Scoring and Selection

### Combined Scoring

```typescript
interface ScoredItem {
  id: string;
  type: "event" | "chunk" | "entity" | "relation" | "pattern";
  content: string;
  tokens: number;
  scores: {
    recency: number;
    relevance: number;
    salience: number;
  };
  finalScore: number;
}

function scoreResults(
  results: {
    recent: ScoredEvent[];
    semantic: ScoredChunk[];
    entities: ScoredEntity[];
    relations: ScoredRelation[];
    patterns: Pattern[];
  },
  signals: QuerySignals,
  strategy: ContextStrategy,
): ScoredItem[] {
  const items: ScoredItem[] = [];

  // Convert all results to common format and score
  for (const event of results.recent) {
    items.push({
      id: event.id,
      type: "event",
      content: formatEvent(event),
      tokens: event.tokens,
      scores: {
        recency: event.score,
        relevance: calculateRelevance(event, signals),
        salience: 0.5, // Default for events
      },
      finalScore: 0, // Calculated below
    });
  }

  // ... similar for other types

  // Calculate final score using strategy weights
  for (const item of items) {
    item.finalScore =
      item.scores.recency * strategy.recencyWeight +
      item.scores.relevance * strategy.relevanceWeight +
      item.scores.salience * strategy.salientWeight;
  }

  // Sort by final score
  return items.sort((a, b) => b.finalScore - a.finalScore);
}
```

### Budget Selection

```typescript
function selectWithinBudget(
  items: ScoredItem[],
  maxTokens: number,
): ScoredItem[] {
  const selected: ScoredItem[] = [];
  let usedTokens = 0;

  // Greedy selection by score
  for (const item of items) {
    if (usedTokens + item.tokens <= maxTokens) {
      selected.push(item);
      usedTokens += item.tokens;
    }
  }

  return selected;
}
```

## Formatting

### Context Structure

```typescript
function formatContext(items: ScoredItem[], request: ContextRequest): string {
  const sections: string[] = [];

  // Group by type
  const byType = groupBy(items, "type");

  // Recent events section
  if (byType.event?.length) {
    sections.push(
      formatSection(
        "Recent Activity",
        byType.event.map((i) => i.content),
      ),
    );
  }

  // Relevant code/content section
  if (byType.chunk?.length) {
    sections.push(
      formatSection(
        "Relevant Content",
        byType.chunk.map((i) => i.content),
      ),
    );
  }

  // Entity context section
  if (byType.entity?.length) {
    sections.push(
      formatSection(
        "Known Entities",
        byType.entity.map((i) => i.content),
      ),
    );
  }

  // Related context section
  if (byType.relation?.length) {
    sections.push(
      formatSection(
        "Relationships",
        byType.relation.map((i) => i.content),
      ),
    );
  }

  // Applicable patterns section
  if (byType.pattern?.length) {
    sections.push(
      formatSection(
        "Applicable Patterns",
        byType.pattern.map((i) => i.content),
      ),
    );
  }

  // Additional context
  if (request.additionalContext) {
    sections.push(
      formatSection("Additional Context", [request.additionalContext]),
    );
  }

  return sections.join("\n\n");
}

function formatSection(title: string, items: string[]): string {
  return `## ${title}\n\n${items.join("\n\n")}`;
}
```

### Item Formatters

```typescript
function formatEvent(event: Event): string {
  const timestamp = new Date(event.timestamp).toISOString();
  const outcome = event.outcome ? ` [${event.outcome}]` : "";

  switch (event.type) {
    case "file_read":
      return `[${timestamp}] Read: ${event.content.path}`;

    case "file_write":
      return `[${timestamp}] Wrote: ${event.content.path}${outcome}`;

    case "shell_exec":
      return `[${timestamp}] Executed: \`${event.content.command}\`${outcome}\n${
        event.content.stdout || event.content.stderr || ""
      }`;

    case "error":
      return `[${timestamp}] Error: ${event.content.message}\n${event.content.stack || ""}`;

    default:
      return `[${timestamp}] ${event.type}: ${JSON.stringify(event.content)}`;
  }
}

function formatEntity(entity: Entity): string {
  const props = Object.entries(entity.properties)
    .map(([k, v]) => `${k}: ${v}`)
    .join(", ");

  return `**${entity.type}**: ${entity.name}${props ? ` (${props})` : ""}`;
}

function formatRelation(relation: Relation): string {
  return `${relation.sourceId} --[${relation.type}]--> ${relation.targetId}`;
}
```

## Token Management

```typescript
class TokenBudget {
  private total: number;
  private used: number = 0;
  private allocations: Map<string, number> = new Map();

  constructor(total: number) {
    this.total = total;
  }

  allocate(percentage: number): number {
    return Math.floor(this.total * (percentage / 100));
  }

  use(tokens: number): void {
    this.used += tokens;
  }

  remaining(): number {
    return this.total - this.used;
  }

  isExceeded(): boolean {
    return this.used > this.total;
  }
}

function estimateTokens(text: string): number {
  // Rough estimate: ~4 characters per token
  return Math.ceil(text.length / 4);
}
```

## Compression

When context exceeds budget, compress less important items:

```typescript
async function compressContext(
  items: ScoredItem[],
  targetTokens: number,
  llm: LLM,
): Promise<ScoredItem[]> {
  // Sort by score (lowest first for compression)
  const sorted = [...items].sort((a, b) => a.finalScore - b.finalScore);

  let currentTokens = items.reduce((sum, i) => sum + i.tokens, 0);

  for (const item of sorted) {
    if (currentTokens <= targetTokens) break;

    // Summarize item content
    const summary = await llm.summarize(item.content, {
      maxTokens: Math.floor(item.tokens / 3), // Target 1/3 of original
    });

    const savedTokens = item.tokens - estimateTokens(summary);
    item.content = summary;
    item.tokens = estimateTokens(summary);
    currentTokens -= savedTokens;
  }

  return items;
}
```

## Performance Optimization

### Caching

```typescript
class ContextCache {
  private cache = new Map<string, { context: Context; timestamp: number }>();
  private ttl = 30000; // 30 seconds

  get(key: string): Context | null {
    const entry = this.cache.get(key);
    if (!entry) return null;
    if (Date.now() - entry.timestamp > this.ttl) {
      this.cache.delete(key);
      return null;
    }
    return entry.context;
  }

  set(key: string, context: Context): void {
    this.cache.set(key, { context, timestamp: Date.now() });
  }

  generateKey(request: ContextRequest): string {
    return `${request.sessionId}:${request.query}:${request.maxTokens}`;
  }
}
```

### Parallel Queries

All memory queries run in parallel (see `Promise.all` in assembly algorithm).

### Incremental Updates

For streaming contexts (e.g., during long conversations):

```typescript
async function updateContext(
  existing: Context,
  newEvent: Event,
  budget: number,
): Promise<Context> {
  // Add new event to beginning
  const newComponent: ContextComponent = {
    type: "recent",
    source: newEvent.id,
    content: formatEvent(newEvent),
    tokens: estimateTokens(formatEvent(newEvent)),
    relevanceScore: 1.0, // New events are highly relevant
  };

  // Check if we need to drop something
  const totalTokens = existing.tokenCount + newComponent.tokens;

  if (totalTokens <= budget) {
    return {
      ...existing,
      components: [newComponent, ...existing.components],
      tokenCount: totalTokens,
    };
  }

  // Drop lowest-scoring component
  const sorted = [...existing.components].sort(
    (a, b) => a.relevanceScore - b.relevanceScore,
  );
  sorted.pop();

  return {
    ...existing,
    components: [newComponent, ...sorted],
    tokenCount: budget,
    truncated: true,
  };
}
```

## Stack-Based Context (Advanced)

> See: [research/context-stack-architecture.md](../research/context-stack-architecture.md)

Traditional linear context (compress last N messages) loses structure. Stack-based context preserves task hierarchy and enables natural "popping" of completed work.

### Task Frame Model

```typescript
interface TaskFrame {
  id: string;
  goal: string;
  parent?: string;
  children: string[];
  status: "active" | "complete" | "blocked";
  result?: string; // Summary when popped
  depth: number;
}

interface TaskStack {
  frames: TaskFrame[];
  current: string;

  push(goal: string): TaskFrame;
  pop(): string; // Returns result summary
  peek(): TaskFrame;
  path(): TaskFrame[]; // Root to current
}
```

### Stack-Aware Assembly

When assembling context with a task stack:

```typescript
function assembleStackContext(
  stack: TaskStack,
  memory: Memory,
  budget: number,
): Context {
  const current = stack.peek();
  const path = stack.path();

  // 1. Root goal (always include) - ~5% budget
  const rootContext = path[0].goal;

  // 2. Stack path summaries - ~15% budget
  // For each ancestor: complete=result, active=goal
  const pathContext = path
    .slice(1, -1)
    .map((f) => (f.status === "complete" ? f.result : f.goal));

  // 3. Sibling results - ~10% budget
  // Completed tasks at same level (context for current work)
  const siblings = stack.frames.filter(
    (f) => f.parent === current.parent && f.status === "complete",
  );
  const siblingResults = siblings.map((f) => f.result);

  // 4. Current frame full detail - remaining budget
  // This is where standard assembly kicks in
  const currentBudget =
    budget -
    estimateTokens([rootContext, ...pathContext, ...siblingResults].join("\n"));

  const currentContext = await buildContext(memory, {
    query: current.goal,
    maxTokens: currentBudget,
    filters: { taskId: current.id },
  });

  return combineContexts(
    rootContext,
    pathContext,
    siblingResults,
    currentContext,
  );
}
```

### Why Stack > Linear

| Linear Model              | Stack Model                     |
| ------------------------- | ------------------------------- |
| Compress last N messages  | Pop completed subtasks          |
| Lose structure            | Preserve hierarchy              |
| Context rot accumulates   | Natural cleanup on completion   |
| "What was I doing?"       | Clear goal path to root         |
| Summarization is lossy    | Results are intentional outputs |
| All context equally stale | Depth = relevance               |

### Integration Points

1. **Episodic Memory**: Stores full task tree (schema extended with `parent_id`, `result`)
2. **Relational Memory**: Task→subtask edges in Graphology graph
3. **Working Memory**: Active stack frames (in-memory)
4. **Context Assembly**: Stack-aware retrieval as shown above

### SOTA References

- **ContextBranch** (Dec 2024): Git-style branching, 58% context reduction
- **THREAD**: Recursive spawning with phi/psi functions for child context/returns
- **Factory.ai Breadcrumbs**: Lightweight pointers with full artifact retrieval
- **HTN Planning**: Hierarchical task decomposition for agent planning

## References

- [memory-architecture.md](memory-architecture.md) - Storage layer
- [archetypes.md](archetypes.md) - Domain customization
- [api-design.md](api-design.md) - Public API
- [../research/context-stack-architecture.md](../research/context-stack-architecture.md) - Stack-based research
