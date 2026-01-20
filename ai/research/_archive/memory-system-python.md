# Memory System Architecture

**Purpose**: Enable continuous agent work without context restart via intelligent memory + context management

**Validated**: 60% reduction in tool calls (7.5 → 3.0 calls/task)

## The Problem

Current agents (Claude Code, etc.):
- Context fills up (100K-200K tokens) → must restart
- Lose track of what was done → repeat work
- No cross-session learning → same mistakes

**Our solution**: 3-layer memory with dynamic pruning

## Three Memory Layers

### 1. DuckDB - Episodic Memory

**Purpose**: Track what agent has done, learn patterns

**Location**: `src/aircher/memory/duckdb_memory.py`

**Tables**:

```sql
-- Tool execution history
tool_executions (
    id, timestamp, session_id, task_id,
    tool_name, parameters, result, success,
    error_message, duration_ms
)

-- File interaction tracking
file_interactions (
    id, timestamp, session_id, task_id,
    file_path, operation, line_range,
    success, context, changes_summary
)

-- Learned patterns (co-editing, error fixes)
learned_patterns (
    id, pattern_type, pattern_data,
    confidence, observed_count,
    first_seen, last_seen
)
```

**Key Queries**:

| Query | Purpose |
|-------|---------|
| `query_task_history(keywords)` | Find similar past tasks |
| `query_error_patterns(limit)` | Get common error fixes |
| `record_tool_execution(...)` | Track what we did |
| `record_successful_fix(...)` | Learn from fixes |

### 2. ChromaDB - Semantic Memory

**Purpose**: Vector search for code similarity

**Location**: `src/aircher/memory/chroma_memory.py`

**Collections**:
- `code_chunks`: Embedded code snippets
- `task_descriptions`: Past task embeddings

**Key Operations**:

| Method | Purpose |
|--------|---------|
| `add_code_chunk(path, content)` | Index code |
| `search_similar(query, k)` | Find relevant code |
| `get_relevant_context(task)` | Pre-fetch for planning |

**Embedding Model**: sentence-transformers (all-MiniLM-L6-v2)

### 3. Knowledge Graph - Structural Memory

**Purpose**: Understand codebase structure and relationships

**Location**: `src/aircher/memory/knowledge_graph.py`

**Nodes**:
- File, Function, Class, Import, Variable

**Edges**:
- Contains, Calls, Imports, Uses, Inherits, References

**Built with**: tree-sitter parsing

**Key Queries**:

| Method | Purpose |
|--------|---------|
| `get_file_contents(path)` | Functions/classes in file |
| `get_callers(function)` | What calls this |
| `get_dependencies(file)` | File dependencies |
| `find_symbol(name)` | Where is this defined |
| `get_relevant_structure(keywords)` | Files matching keywords |

## Memory Integration

**Location**: `src/aircher/memory/integration.py`

Unified facade combining all three layers:

```python
class MemoryIntegration:
    def query_for_planning(self, task: str) -> PlanningContext:
        """Query all layers for task planning."""
        similar_tasks = self.duckdb.query_task_history(task)
        error_patterns = self.duckdb.query_error_patterns()
        relevant_code = self.chroma.search_similar(task)
        structure = self.knowledge_graph.get_relevant_structure(task)
        return PlanningContext(...)

    def suggest_correction(self, error: str) -> Optional[str]:
        """Find past fix for similar error."""
        return self.duckdb.find_similar_error_fix(error)
```

## Dynamic Context Management

**Location**: `src/aircher/context/window.py`

### Pruning Algorithm

```python
def calculate_relevance(item: ContextItem) -> float:
    score = 1.0

    # Time decay (exponential, ~1 hour half-life)
    age_minutes = (now - item.timestamp).minutes
    score *= exp(-age_minutes / 60.0)

    # Task association (2x boost for current task)
    if item.task_id == current_task_id:
        score *= 2.0

    # Type multiplier
    type_weights = {
        "system_prompt": 100.0,  # Never remove
        "task_state": 2.0,
        "user_message": 1.5,
        "tool_result": 0.8,
        "code_snippet": 0.7,
    }
    score *= type_weights.get(item.type, 1.0)

    return score
```

**Pruning triggers at 80% capacity** (120k/150k tokens)

### Context Preparation

Before each LLM call:
1. Prune if needed
2. Query knowledge graph for relevant code
3. Query episodic memory for similar tasks
4. Build message list with relevance ordering

## Storage Layout

### Current (Global)

```
~/.aircher/
├── data/
│   ├── sessions.db      # SQLite
│   ├── episodic.duckdb  # DuckDB
│   └── vectors/         # ChromaDB
└── config/
```

### Target (Phase 7 - Project-Scoped)

```
~/.aircher/
├── global/              # Universal patterns
│   └── error_patterns.duckdb
├── projects/            # Per-project
│   └── <git_root_hash>/
│       ├── episodic.duckdb
│       ├── vectors/
│       └── learned/     # Agent-managed
│           ├── patterns.md
│           └── errors.md
└── config/
```

## Validation Results

| Metric | Before | After | Improvement |
|--------|--------|-------|-------------|
| Tool calls/task | 7.5 | 3.0 | **60%** |
| File re-reads | High | Low | Tracked |
| Error repetition | Common | Rare | Remembered |

## Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| DuckDB over SQLite | Better for analytical queries, JSON columns |
| ChromaDB for vectors | Specialized, embedding support |
| NetworkX for graph | Python native, good for codebase size |
| 80% pruning threshold | Leave buffer for response generation |
| Time-decay relevance | Recent context more important |

## References

- **context-engineering-concepts.md**: Session vs context, compaction strategies
- **ai/DESIGN.md**: System architecture overview
- **src/aircher/memory/**: Implementation code
