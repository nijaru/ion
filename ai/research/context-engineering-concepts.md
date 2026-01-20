# Context Engineering for AI Agents

**Sources**:
- [Google Whitepaper: Context Engineering - Sessions and Memory](https://www.kaggle.com/whitepaper-context-engineering-sessions-and-memory)
- [Manus Blog: Context Engineering for AI Agents](https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus)

## Key Concepts

| Term | Definition |
|------|------------|
| **Session** | Persistent log of all interactions (events + state) |
| **Context** | Curated payload sent to LLM (subset of session) |
| **Memory** | Long-term knowledge extracted across sessions |

**Critical distinction**: Session history ≠ Context. Session is permanent transcript; context is carefully selected.

## Core Challenges

1. **Ever-growing history**: Cost/latency increases with context size
2. **Context rot**: Attention to important info diminishes as context expands

**Solution**: Dynamic history mutation via summarization, pruning, compaction

## Session Management

### Multi-Agent Approaches

| Approach | When to Use |
|----------|-------------|
| **Shared unified history** | Tightly coupled, collaborative tasks |
| **Separate individual histories** | Agent-as-tool, A2A protocols |

**Problem with A2A**: Fails to share rich contextual state. Solution: Memory layer as universal common storage.

### Compaction Strategies

| Strategy | Description |
|----------|-------------|
| Last N turns | Keep recent, discard older |
| Token truncation | Max messages within token limit |
| Recursive summarization | Replace old with AI summaries |

### Manus Insights (Peak Ji)

1. **File system as ultimate context**
   - Compression must be **restorable**
   - Drop content but keep URLs/paths
   - Can always re-fetch from preserved references

2. **Recitation for attention**
   - Constantly rewrite todo lists
   - Push objectives into recent context
   - Avoids "lost-in-the-middle" problem

3. **Keep failures in context**
   - Don't erase hallucinations or failed tool calls
   - Model learns from seeing failed attempts
   - Erasing removes evidence for adaptation

4. **Avoid few-shot repetition**
   - Similar past action-observation pairs cause pattern lock
   - Increase diversity with controlled randomness

### Compaction Triggers

| Type | When |
|------|------|
| Count-based | Token/message threshold |
| Time-based | User inactivity period |
| Event-based | Task/topic completion |

## Memory Architecture

### Five Components

```
1. User → raw data source
2. Agent → what/when to remember (simple: always, advanced: memory-as-tool)
3. Framework → structure/tools (LangGraph, ADK)
4. Session Storage → turn-by-turn (SQLite, Redis)
5. Memory Manager → extraction, consolidation, retrieval (Mem0, Zep)
```

### Memory vs RAG

Memory managers handle **contextual, evolving information** with:
- Conflict resolution
- Semantic understanding for conversations
- Provenance tracking

### Storage Options

| Type | Use Case |
|------|----------|
| Vector DB | Semantic similarity, natural language |
| Knowledge Graph | Structured relationships, entity connections |

## Memory Generation Pipeline

```
Ingestion → Extraction → Consolidation → Storage
```

### 1. Extraction

**Key question**: What is meaningful enough to become memory?

- LLM decides based on system prompt guardrails
- Predefined JSON schema/template
- Few-shot examples of extraction patterns

### 2. Consolidation (Critical)

Handles:
- **Duplication**: Prevent redundant memories
- **Conflicts**: Resolve contradictions
- **Evolution**: Facts become more nuanced
- **Decay**: TTL pruning of stale memories

**Process**: LLM "self-editing" - compare new with existing, decide to merge/delete/create

### 3. Provenance

Track memory origin for trustworthiness:

| Source | Trust Level |
|--------|-------------|
| Bootstrapped | Initialize, address cold-start |
| User input | Explicit, high trust |
| Tool output | Brittle, avoid for long-term |

### Memory-as-Tool

Advanced pattern: Agent decides when to create/retrieve memory.

```python
# Tool definition specifies meaningful info types
# Agent auto-decides tool invocation
@tool
def save_memory(content: str, category: str):
    """Save meaningful information for future reference."""
    ...
```

## Inference with Memories

### Placement Options

| Location | Pros | Cons |
|----------|------|------|
| System prompt | High authority, clean separation | Over-influence, incompatible with memory-as-tool |
| Conversation history | Flexible, natural flow | Noise, dialogue injection risk |

**Tip**: Write memories in first-person to avoid injection.

## Evaluation Framework

### 1. Memory Generation Quality

| Metric | What it Measures |
|--------|------------------|
| Precision | % of created memories that are accurate/relevant |
| Recall | % of relevant facts captured from source |
| F1 | Balanced precision-recall |

### 2. Retrieval Performance

| Metric | What it Measures |
|--------|------------------|
| Recall@K | Correct memory in top K results |
| Latency | Must fit latency budget |

### 3. End-to-End

Overall task success with memory integration.

## Implications for Aircher

### Already Implemented

| Concept | Aircher Implementation |
|---------|------------------------|
| Session vs Context | ContextWindow with pruning |
| 3-layer memory | DuckDB + ChromaDB + Knowledge Graph |
| Compaction | Token threshold (80% capacity) |

### Phase 7 Additions Needed

| Concept | Action |
|---------|--------|
| **Restorable compression** | Keep file paths when dropping content |
| **Memory consolidation** | Add conflict resolution, TTL decay |
| **Memory provenance** | Track source (bootstrap/user/tool) |
| **Recitation** | Rewrite objectives into recent context |
| **Keep failures** | Don't erase failed tool calls |
| **Memory-as-tool** | Let agent decide when to save |

### Evaluation Gaps

| Missing | Action |
|---------|--------|
| Memory precision/recall | Add extraction quality metrics |
| Retrieval@K | Measure memory retrieval accuracy |
| End-to-end with memory | Benchmark with vs without memory |

## Key Takeaways

1. **Session ≠ Context**: Maintain full history, curate what's sent
2. **Compression must be restorable**: Keep references, drop content
3. **Consolidation is critical**: Conflict resolution, dedup, decay
4. **Keep failures visible**: Model learns from seeing mistakes
5. **Evaluate systematically**: Precision/recall for extraction, Recall@K for retrieval
