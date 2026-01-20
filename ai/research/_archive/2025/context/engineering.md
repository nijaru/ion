# Context Engineering for AI Agents

> Synthesis of research on AI agent memory, context management, and decision systems (Dec 2025).

**Sources**:

- [Google Whitepaper: Context Engineering - Sessions and Memory](https://www.kaggle.com/whitepaper-context-engineering-sessions-and-memory)
- [Manus Blog: Context Engineering for AI Agents](https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus)
- [Chroma Context Rot](https://research.trychroma.com/context-rot)
- [ACE Paper: arXiv:2510.04618](https://arxiv.org/abs/2510.04618)
- [Zep Temporal Graph: arXiv:2501.13956](https://arxiv.org/abs/2501.13956)

---

## Core Concepts

| Term        | Definition                                          |
| ----------- | --------------------------------------------------- |
| **Session** | Persistent log of all interactions (events + state) |
| **Context** | Curated payload sent to LLM (subset of session)     |
| **Memory**  | Long-term knowledge extracted across sessions       |

**Critical distinction**: Session history ≠ Context. Session is permanent transcript; context is carefully selected.

### Five Components

```
1. User → raw data source
2. Agent → what/when to remember (simple: always, advanced: memory-as-tool)
3. Framework → structure/tools (LangGraph, ADK)
4. Session Storage → turn-by-turn (SQLite, Redis)
5. Memory Manager → extraction, consolidation, retrieval (Mem0, Zep)
```

---

## Context Rot (Chroma Research)

### Methodology

18 LLMs tested with 194k+ API calls across 5 experiment types:

- Needle-in-Haystack with semantic similarity variations
- LongMemEval (113k tokens full vs 300 tokens focused)
- Distractor impact analysis
- Haystack structure (logical vs shuffled)
- Repeated words task

### Key Findings

| Finding                                                           | Implication                             |
| ----------------------------------------------------------------- | --------------------------------------- |
| Low needle-question similarity → performance cliff at 10k+ tokens | Query-answer semantic alignment matters |
| Even 1 distractor causes performance drop                         | Aggressive filtering needed             |
| **Shuffled text > logical organization**                          | Structure disperses attention           |
| Time-change distractors worst                                     | Temporal consistency critical           |

### Model-Specific Behaviors

| Model  | Hallucination | Behavior                                       |
| ------ | ------------- | ---------------------------------------------- |
| Claude | Lowest        | Abstains when uncertain ("cannot find answer") |
| GPT    | Highest       | Confident when wrong                           |
| Gemini | Medium        | Random words at 500-750 tokens                 |
| Qwen   | Medium        | Random output after 5k words                   |

---

## Agentic Context Engineering (ACE)

### The Problem

**Context collapse**: Repeated updates compress and destroy information. Agents "forget as fast as they learn" due to:

- Brevity bias (discarding domain knowledge for conciseness)
- Detail erosion during iterative rewrites
- No quality control on what enters context

### Three-Module Architecture

```
Generator → Reflector → Curator
    ↓           ↓           ↓
trajectories  insights   delta updates
```

**Generator**: Produces reasoning, marks which context bullets helped/hindered.
**Reflector**: Extracts lessons without modifying context (separation of concerns).
**Curator**: Converts insights to deltas, uses deterministic merging.

### Delta Update Mechanism

Bullets with metadata:

```
[section-ID] helpful=X harmful=Y :: content
```

Key properties:

1. **Localization**: Only relevant bullets update
2. **Fine-grained retrieval**: Generator focuses on pertinent knowledge
3. **Incremental adaptation**: Semantic deduplication during merge

### Results

| Benchmark         | Improvement     |
| ----------------- | --------------- |
| AppWorld (agents) | +17.1% accuracy |
| Finance (FiNER)   | +8.6% accuracy  |
| Latency (offline) | -82.3%          |
| Token cost        | -83.6%          |

ACE with smaller open-source models matched GPT-4.1 performance.

---

## Context Graphs & Decision Traces

### The Missing Layer

Systems of record capture **what** (objects, state) but not **why** (decisions, exceptions, precedents).

> "Agents don't just need rules. They need access to the decision traces that show how rules were applied in the past, where exceptions were granted, how conflicts were resolved."

### What's Not Captured

| Category               | Example                                                       |
| ---------------------- | ------------------------------------------------------------- |
| Exception logic        | "We always give healthcare 10% because procurement is brutal" |
| Precedent              | "We structured a similar deal last quarter—be consistent"     |
| Cross-system synthesis | Support lead checks CRM, Zendesk, Slack, decides to escalate  |
| Approval chains        | VP approves discount in Slack DM, system shows final price    |

### Context Graph

A **queryable record of how decisions were made**:

- Entities (accounts, tickets, policies, approvers)
- Decision events (the moments that matter)
- "Why" links (precedent, exception, approval)

---

## Compaction Best Practices (Manus/Momo)

### KV-Cache Optimization

Critical for agents (huge prefills, tiny decodes). Cache breaks from:

- Timestamps in system prompt
- Modified tool definitions
- Non-deterministic JSON serialization
- Inserted/removed prefix lines

### Recommended Patterns

| Pattern                  | Rationale                                 |
| ------------------------ | ----------------------------------------- |
| Append-only context      | Never rewrite earlier messages            |
| Tool masking             | Keep tools in context, hide via parameter |
| Filesystem-as-context    | URLs not content, restore on demand       |
| Preserve failures        | Model needs evidence to adapt             |
| Recite objectives at end | Focus recent attention span               |

### Anti-Patterns

- Dynamic tool loading (causes hallucination)
- Few-shot that becomes suboptimal
- Erasing hallucinations (prevents learning)
- Aggressive compaction (information loss)

### Compaction Strategies

| Strategy                | Description                     |
| ----------------------- | ------------------------------- |
| Last N turns            | Keep recent, discard older      |
| Token truncation        | Max messages within token limit |
| Recursive summarization | Replace old with AI summaries   |

### Compaction Triggers

| Type        | When                    |
| ----------- | ----------------------- |
| Count-based | Token/message threshold |
| Time-based  | User inactivity period  |
| Event-based | Task/topic completion   |

---

## Zep Temporal Knowledge Graph

### Architecture

Graphiti engine: temporally-aware knowledge graph synthesizing conversation + business data.

### Results

| Benchmark   | Score                   |
| ----------- | ----------------------- |
| DMR         | 94.8% (vs MemGPT 93.4%) |
| LongMemEval | +18.5% accuracy         |
| Latency     | -90% reduction          |

### Key Innovation

Temporal awareness enables cross-session synthesis and historical context maintenance.

---

## Memory Architecture

### Memory vs RAG

Memory managers handle **contextual, evolving information** with:

- Conflict resolution
- Semantic understanding for conversations
- Provenance tracking

### Storage Options

| Type            | Use Case                                     |
| --------------- | -------------------------------------------- |
| Vector DB       | Semantic similarity, natural language        |
| Knowledge Graph | Structured relationships, entity connections |

### Memory-as-Tool

Advanced pattern: Agent decides when to create/retrieve memory.

```python
@tool
def save_memory(content: str, category: str):
    """Save meaningful information for future reference."""
    ...
```

### Provenance

Track memory origin for trustworthiness:

| Source       | Trust Level                    |
| ------------ | ------------------------------ |
| Bootstrapped | Initialize, address cold-start |
| User input   | Explicit, high trust           |
| Tool output  | Brittle, avoid for long-term   |

---

## Evaluation Framework

### Memory Generation Quality

| Metric    | What it Measures                                 |
| --------- | ------------------------------------------------ |
| Precision | % of created memories that are accurate/relevant |
| Recall    | % of relevant facts captured from source         |
| F1        | Balanced precision-recall                        |

### Retrieval Performance

| Metric   | What it Measures        |
| -------- | ----------------------- |
| Recall@K | Correct memory in top K |
| Latency  | Must fit latency budget |

### End-to-End

Overall task success with memory integration.

---

## Priority Changes for Aircher

### High Priority

| Change                                   | Rationale                | Effort |
| ---------------------------------------- | ------------------------ | ------ |
| Add helpful/harmful counters to entities | ACE showed +17% accuracy | Low    |
| Query-needle similarity threshold        | Context rot research     | Medium |
| Tool masking in archetype                | KV-cache optimization    | Low    |

### Medium Priority

| Change                       | Rationale               | Effort |
| ---------------------------- | ----------------------- | ------ |
| Reflector-Curator separation | ACE architecture        | Medium |
| Decision event type          | Enterprise audit trails | Low    |
| Objectives recitation        | Attention focus         | Low    |

### Research Validated (No Change Needed)

- Budget-based context assembly
- Append-only episodic store
- Outcome tracking (success/failure)
- Artifacts section for files
- Bi-temporal schema

---

## Key Takeaways

1. **Session ≠ Context**: Maintain full history, curate what's sent
2. **Compression must be restorable**: Keep references, drop content
3. **Consolidation is critical**: Conflict resolution, dedup, decay
4. **Keep failures visible**: Model learns from seeing mistakes
5. **Delta updates beat full rewrites**: +17% accuracy, -83% cost
6. **Shuffled > logical**: Counterintuitive but data-backed
7. **Claude abstains when uncertain**: Lowest hallucination rate
