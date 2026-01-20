# Memory/Persistence Architectures in AI Coding Agents

**Date**: 2026-01-12
**Purpose**: Compare memory approaches to identify OmenDB differentiation

---

## Comparison Table

| System             | Storage                      | Context Assembly                  | Cross-Session    | Memory Types                            | Integration     |
| ------------------ | ---------------------------- | --------------------------------- | ---------------- | --------------------------------------- | --------------- |
| **OmenDB Memory**  | SQLite + vectors (OmenDB)    | Budget-aware, selective retrieval | Yes              | Semantic, Episodic, Working, Procedural | MCP, SDK        |
| **Goose**          | JSON files (KG-MCP)          | Full injection at session start   | Yes (tag-based)  | Tag-based facts                         | MCP extension   |
| **Letta (MemGPT)** | PostgreSQL + vectors         | Self-editing in-context blocks    | Yes              | Core (Human/Persona), Archival          | Native stateful |
| **Pieces**         | Local SQLite + LTM engine    | 9-month rolling context           | Yes              | Snippets, Activity, LTM                 | Native + MCP    |
| **Claude Code**    | None (session only)          | None                              | No               | None                                    | Native tools    |
| **Aider**          | None (generates per-session) | Tree-sitter repo map              | No (regenerated) | Structural (AST)                        | Native          |

---

## Detailed Analysis

### 1. OmenDB Memory (Our System)

**Storage**:

- SQLite for relational data (agents, workspaces, tasks)
- OmenDB/vector providers (Voyage, Turbopuffer) for semantic search
- Local-first with optional cloud sync

**Context Assembly** (Key Innovation):

```
Query -> needsMemory() check -> Hybrid Retrieval (RRF) -> Scoring Pipeline -> Budget Fill
                                       |                        |
                          Semantic + Temporal + Entity    ACE + Time Decay + Type Weights
```

- **Budget-aware**: Fills context up to `maxTokens`, stops when budget exhausted
- **Selective retrieval**: `needsMemory()` skips transactional queries entirely
- **Multi-strategy fusion**: RRF merges semantic, temporal, entity-based results
- **SOTA scoring**: ACE (+17% accuracy), time decay (type-specific), deduplication (0.92 cosine)

**Memory Types**:
| Type | Half-Life | Weight | Purpose |
|------|-----------|--------|---------|
| Persona | N/A | 100.0 | Always included first |
| Semantic | 30 days | 1.5 | Stable facts/knowledge |
| Episodic | 7 days | 1.0 | Events, what happened |
| Working | 1 hour | 2.0 | Current session context |
| Procedural | 90 days | 1.2 | Learned behaviors |

**Unique Features**:

- Bi-temporal queries (validAt for point-in-time retrieval)
- ACE helpful/harmful counters for memory quality tracking
- Entity extraction for graph-like retrieval
- Query classification to prevent memory over-weighting

---

### 2. Goose (Block Labs)

**Storage**:

- Simple JSON files via Knowledge Graph MCP Server
- `@modelcontextprotocol/server-memory` package
- Local filesystem persistence

**Context Assembly**:

- **Full injection**: All saved memories loaded at session start
- **Included in every prompt**: No selective retrieval
- Large memories stored in files, referenced by path

**Memory Types**:

- Tag-based organization only
- No distinction between semantic/episodic
- User teaches facts via natural language

**Integration**:

- MCP extension (built-in or external)
- Chat Recall extension for session history

**Limitations**:

- No token budget awareness
- No relevance scoring
- Full context injection wastes tokens on irrelevant memories
- No time decay or priority

**Quote from docs**: "goose loads all saved memories at the start of a session and includes them in every prompt sent to the LLM"

---

### 3. Letta (MemGPT)

**Storage**:

- PostgreSQL with pgvector
- Separate in-context and out-of-context tiers
- Cloud-hosted (Letta Platform) or self-hosted

**Context Assembly**:

- **Self-editing**: Agent modifies its own memory blocks
- **Two-tier hierarchy**:
  - In-context: Core memory blocks (Human, Persona) + conversation history
  - Out-of-context: Archival storage, evicted history

**Memory Types**:

- **Core Memory Blocks**: Structured, in-context, agent-editable
  - Human block: User info, preferences
  - Persona block: Agent identity, instructions
- **Archival Memory**: Vector-searchable long-term storage
- **Conversation History**: Recent messages, evicted to archival

**Integration**:

- Native stateful agents
- Python/TypeScript SDK
- REST API
- ADE (Agent Development Environment) for debugging

**Architecture** (from MemGPT paper):

```
Context Window (limited)
├── System Instructions
├── Core Memory Blocks (editable by agent)
│   ├── Persona
│   └── Human
├── Recent Conversation
└── Retrieved Archival (on demand)
```

**Key Innovation**: Agent decides what to remember via `core_memory_append`, `core_memory_replace` tools

**Limitations**:

- Requires server (not local-first)
- Higher latency from stateful loop
- More complex than simple injection

---

### 4. Pieces for Developers

**Storage**:

- Local SQLite ("Long-Term Memory" engine)
- 9-month rolling context window
- Local-first, no cloud required

**Context Assembly**:

- **Activity-aware**: Tracks what you work on
- **Snippet management**: Code snippets with metadata
- **Workstream context**: Understands current project state

**Memory Types**:

- **Code Snippets**: Saved code with annotations
- **Activity Stream**: What files/tools you used
- **Long-Term Memory**: Extracted facts over 9 months

**Integration**:

- Native IDE plugins (VS Code, JetBrains)
- MCP Server for Claude/Cursor
- Browser extension
- Desktop app

**Unique Features**:

- Automatic context capture from IDE activity
- Snippet organization and tagging
- Cross-IDE sync

**Positioning**: "AI second brain" - memory augments code completion, not replaces it

---

### 5. Claude Code (Anthropic)

**Storage**: None

**Context Assembly**: None (session-only)

**Memory Types**: None

**Integration**: Native tools (Read, Write, Bash, Grep, Glob)

**Design Philosophy** (from Anthropic engineering blog):

- "Simplicity through constraint"
- Single main thread, flat message list
- No memory = no retrieval overhead
- Regex search (GrepTool) instead of vector DB

**Quote**: "Claude already understands code structure deeply enough to craft sophisticated regex patterns"

**Implications**:

- Fast, predictable, debuggable
- No cross-session learning
- No personalization
- Relies on CLAUDE.md files for project context

---

### 6. Aider

**Storage**: None (generated per-session)

**Context Assembly**:

- **Repo Map**: Tree-sitter extracts AST for entire codebase
- Generated fresh each session
- Includes function signatures, class definitions, imports

**Approach** (from Aider blog):

```
Codebase -> Tree-sitter Parse -> AST Extraction -> Concise Map -> Inject to LLM
```

**What's Included**:

- Most important classes and functions
- Types and call signatures
- Import relationships
- Dependencies between files

**Memory Types**:

- Structural only (AST)
- No semantic/episodic distinction
- No learning across sessions

**Unique Innovation**:

- Whole-repo context in limited tokens
- Tree-sitter for multi-language support
- No vector DB needed

**Limitations**:

- No cross-session persistence
- No user preference memory
- Regenerated every session (cold start)

---

## Where OmenDB Differentiates

### 1. Budget-Aware Context Assembly

**Problem with alternatives**:

- Goose: Full injection wastes tokens
- Letta: Self-editing adds latency
- Claude Code: No memory at all
- Aider: Structural only, no semantic

**OmenDB approach**:

```typescript
// Fill context by type priority, respecting token budget
for (const memType of TYPE_ORDER) {
  const typeMemories = scored.filter((m) => m.record.type === memType);
  for (const mem of typeMemories) {
    const memTokens = estimateTokens(mem.record.content);
    if (tokensUsed + memTokens > memoryBudget) continue;
    includedMemories.push(mem);
    tokensUsed += memTokens;
  }
}
```

**Result**: Maximum relevance per token, no wasted context

### 2. Query Classification (needsMemory)

**Problem**: Most systems retrieve memories for every query, even transactional ones.

**OmenDB innovation**:

```typescript
// Skip memory for queries that don't need personalization
const SKIP_MEMORY_PATTERNS = [
  /^(what|how).*(weather|time|date)/i,
  /^(hi|hello|hey|thanks|bye)/i,
  /^(calculate|convert|translate)/i,
];

if (!needsMemory(query)) return []; // Fast path
```

**Result**: Faster responses, fewer irrelevant memories injected

### 3. Research-Backed Scoring

**Unique combination of proven patterns**:

| Technique           | Source           | Impact                  |
| ------------------- | ---------------- | ----------------------- |
| ACE helpful/harmful | arXiv:2510.04618 | +17% accuracy           |
| RRF fusion          | Hindsight paper  | +6% accuracy            |
| Time decay          | Anthropic CE     | Prevents stale memories |
| Bi-temporal         | Zep/Graphiti     | Point-in-time queries   |
| Dedup 0.92          | Mem0/Google      | Removes near-duplicates |

### 4. Local-First with MCP

**Deployment flexibility**:

- Single binary (Bun)
- No external dependencies (SQLite + OmenDB local)
- MCP server for Claude Desktop/Cursor
- SDK for custom integrations

**vs Letta**: Requires PostgreSQL server
**vs Pieces**: Desktop app (not embeddable)
**vs Goose**: Simple JSON (no scoring)

### 5. Multi-Strategy Hybrid Retrieval

```typescript
// Run strategies in parallel, merge with RRF
const [semanticResults, temporalResults, entityResults] = await Promise.all([
  vector.search(queryVector), // Semantic similarity
  vector.search(queryVector, { types: ["episodic", "working"] }), // Recent
  vector.search(queryVector, { entities }), // Entity mentions
]);

return rrfMerge([semanticResults, temporalResults, entityResults]);
```

**No other system combines**: Semantic + Temporal + Entity + RRF fusion

---

## Summary: Competitive Positioning

| Dimension            | OmenDB Advantage                                 |
| -------------------- | ------------------------------------------------ |
| **Token Efficiency** | Budget-aware assembly vs full injection          |
| **Relevance**        | Multi-factor scoring vs simple similarity        |
| **Speed**            | Query classification skips unnecessary retrieval |
| **Accuracy**         | ACE + RRF (+23% combined)                        |
| **Deployment**       | Local-first vs cloud-required                    |
| **Integration**      | MCP + SDK vs single integration pattern          |
| **Research**         | Implements 5+ SOTA techniques                    |

**Key Message**: OmenDB is the only system that combines budget-aware context assembly with research-backed scoring in a local-first package.

---

## References

**OmenDB Implementation**:

- `/Users/nick/github/omendb/memory/packages/server/src/lib/scoring.ts`
- `/Users/nick/github/omendb/memory/packages/server/src/lib/retrieval.ts`
- `/Users/nick/github/omendb/memory/packages/server/src/routes/context.ts`

**External Sources**:

- Goose Memory Extension: https://block.github.io/goose/docs/mcp/memory-mcp/
- Letta MemGPT Architecture: https://docs.letta.com/guides/agents/architectures/memgpt
- Letta Memory Blocks: https://www.letta.com/blog/memory-blocks
- Pieces LTM: https://pieces.app/blog/best-ai-memory-systems
- Aider Repo Map: https://aider.chat/2023/10/22/repomap.html
- Claude Code Architecture: `/Users/nick/github/nijaru/aircher/ai/research/competitive/claude-code.md`

**Research Papers**:

- ACE (arXiv:2510.04618): +17% accuracy with delta updates
- Hindsight TEMPR: RRF fusion for multi-strategy retrieval
- Zep Graphiti (arXiv:2501.13956): Temporal knowledge graphs
- Context Rot (Chroma): Query-needle alignment
