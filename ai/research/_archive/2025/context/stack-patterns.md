# Context Stack Architecture

> Research: Stack-based vs linear context management for AI agents (2025-12-22)

## The Problem

Linear chat history is the wrong data structure for agent work. Engineers think in **call stacks**:

```
main_task
  └─ subtask_1 (complete, popped)
  └─ subtask_2 (active)
       └─ sub_subtask_a (complete, popped)
       └─ sub_subtask_b (active) ← current focus
```

When a subtask completes, you don't need its full trace—just its **result**.

## SOTA Patterns

### 1. ContextBranch (Dec 2024)

Git-style conversation management with 4 primitives:

| Primitive  | Purpose                |
| ---------- | ---------------------- |
| checkpoint | Capture state at point |
| branch     | Isolate exploration    |
| switch     | Navigate branches      |
| inject     | Selective merge        |

**Results**: 58% context reduction (31→13 messages), improved focus.

Source: [arXiv 2512.13914](https://arxiv.org/abs/2512.13914)

### 2. THREAD (Recursive Spawning)

Parent threads spawn children dynamically:

- **φ (phi)**: Defines child context from parent (typically last line)
- **ψ (psi)**: Defines child's return value (typically print statements)
- Child outputs append to parent upon completion (join)

This IS stack semantics: push (spawn), pop (join with summary).

Source: [arXiv 2405.17402](https://arxiv.org/html/2405.17402v1)

### 3. HTN Planning (Hierarchical Task Networks)

Abstract tasks decomposed iteratively:

```
goal → subtasks → sub-subtasks → primitive actions
```

GPT-HTN-Planner, ChatHTN implement this with LLMs. Tracks plan hierarchy, enables re-planning on failure.

### 4. Breadcrumb Compression (Factory.ai)

Restorable compression pattern:

| Kept               | Dropped                   |
| ------------------ | ------------------------- |
| File paths         | Full file contents        |
| Function names     | Implementation details    |
| Key identifiers    | Tool output logs          |
| Retrieval pointers | Reconstructable artifacts |

Agent can query to re-access any dropped content. **Minimize tokens per task, not per request.**

### 5. Compression Evaluation (Factory.ai, 2025)

Probe-based evaluation comparing Factory, OpenAI `/responses/compact`, and Claude SDK.

**Four probe types**:

- **Recall**: Factual retention of details
- **Artifact**: File tracking and modifications
- **Continuation**: Task planning capability
- **Decision**: Reasoning chain preservation

**Key findings**:

| Approach         | Compression | Score  | Notes                                   |
| ---------------- | ----------- | ------ | --------------------------------------- |
| Factory Anchored | 99.3%       | 3.70/5 | Persistent structured summaries         |
| OpenAI Compact   | 99.3%       | 3.35/5 | Opaque but high reconstruction fidelity |
| Claude SDK       | Lower       | Mid    | Full regeneration causes drift          |

**Critical insights**:

- **Structure forces preservation**: Dedicated sections for file paths, decisions prevent silent drops
- **Artifact tracking unsolved**: All methods scored 2.19-2.45/5 on file knowledge
- **Anchored > regenerated**: Merging new summaries beats full regeneration (0.45 point advantage)
- **Accuracy gap largest**: Technical details like paths survive better with structure (4.04 vs 3.43)

Source: [factory.ai/news/evaluating-compression](https://factory.ai/news/evaluating-compression)

### 6. Tree-Sitter Repo Map (Aider)

Whole codebase awareness in ~1K tokens:

1. Parse with tree-sitter → extract symbols
2. Build dependency graph (files → functions)
3. Rank by reference frequency
4. Fit most relevant into budget

Already in aircher's stack (tree-sitter for coding archetype).

### 7. Bi-Temporal Memory (Zep)

Every fact tracks two timestamps:

- **Event time (T)**: When fact occurred
- **Ingestion time (T')**: When observed

Facts **invalidated, not deleted**. Enables temporal queries and audit trails.

## Architecture for Aircher

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
  current: string; // Active frame ID

  push(goal: string): TaskFrame;
  pop(): string; // Returns result summary
  peek(): TaskFrame;
}
```

### Context Assembly (Stack-Aware)

```typescript
function assembleContext(stack: TaskStack, budget: number): Message[] {
  const frames = stack.frames;
  const current = stack.peek();

  // 1. Root goal (always include)
  const rootContext = frames[0].goal;

  // 2. Stack path: goal summaries from root to current
  const pathContext = frames
    .filter((f) => isAncestorOf(f, current))
    .map((f) => (f.status === "complete" ? f.result : f.goal));

  // 3. Sibling results (completed tasks at same level)
  const siblingResults = frames
    .filter((f) => f.parent === current.parent && f.status === "complete")
    .map((f) => f.result);

  // 4. Current frame: full detail
  const currentContext = getCurrentFrameDetail(current);

  return fitToBudget(
    [rootContext, ...pathContext, ...siblingResults, currentContext],
    budget,
  );
}
```

### Integration with Memory Types

| Memory Type                 | Stack Role                              |
| --------------------------- | --------------------------------------- |
| **Episodic** (SQLite)       | Store full task tree (flame graph data) |
| **Relational** (Graphology) | Task→subtask edges, dependency graph    |
| **Working** (In-memory)     | Active stack frames                     |
| **Contextual** (OmenDB)     | Semantic search for similar past tasks  |

### Schema Extension

```sql
-- Task hierarchy in episodic memory
CREATE TABLE tasks (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  parent_id TEXT,
  goal TEXT NOT NULL,
  status TEXT DEFAULT 'active',  -- active, complete, blocked
  result TEXT,                    -- Summary when complete
  depth INTEGER DEFAULT 0,
  created_at INTEGER,
  completed_at INTEGER,
  valid_from INTEGER,             -- Bi-temporal
  valid_to INTEGER,
  FOREIGN KEY (parent_id) REFERENCES tasks(id)
);

-- Index for stack traversal
CREATE INDEX idx_tasks_parent ON tasks(parent_id);
CREATE INDEX idx_tasks_session ON tasks(session_id, status);
```

## Why This Matters

| Linear Model             | Stack Model                   |
| ------------------------ | ----------------------------- |
| Compress last N messages | Pop completed subtasks        |
| Lose structure           | Preserve hierarchy            |
| Context rot accumulates  | Natural cleanup on completion |
| "What was I doing?"      | Clear goal path to root       |
| Summarization is lossy   | Results are intentional       |

## Implementation Priority

1. **Phase 9.1**: Add task hierarchy to episodic memory schema
2. **Phase 9.1**: Context assembly with stack-aware retrieval
3. **Phase 9.3**: Tree-sitter repo map (already planned)
4. **Phase 9.4**: CLI with task stack visualization

## Key Sources

- ContextBranch: [arXiv 2512.13914](https://arxiv.org/abs/2512.13914)
- THREAD: [arXiv 2405.17402](https://arxiv.org/html/2405.17402v1)
- Factory.ai breadcrumbs: [factory.ai/news/compressing-context](https://factory.ai/news/compressing-context)
- Aider repo map: [aider.chat/docs/repomap](https://aider.chat/docs/repomap.html)
- Zep temporal graph: [arXiv 2501.13956](https://arxiv.org/abs/2501.13956)
- MemGPT/Letta: [arxiv.org/abs/2310.08560](https://arxiv.org/abs/2310.08560)
- ACON optimization: [arXiv 2510.00615](https://arxiv.org/html/2510.00615v1)
