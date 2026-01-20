# Design Spec: Budget-Aware Memory Retrieval (OmenDB)

## 1. Goal

Implement a persistent, cross-session memory system for `ion` that retrieves relevant context using vector similarity (OmenDB) and hybrid ranking, while strictly adhering to a token budget.

## 2. Core Components

### 2.1 Embedding Engine

Since `ion` is multi-provider, we need a flexible way to generate query embeddings.

- **Provider**: Use the active provider's embedding endpoint if available (e.g., OpenAI `text-embedding-3-small`).
- **Local Fallback**: Support `Ollama` or a lightweight local model (e.g., `fastembed`) for users who want total privacy.
- **Abstraction**: `trait EmbeddingProvider { async fn embed(&self, text: &str) -> Result<Vec<f32>>; }`

### 2.2 Memory System (Existing + New)

- **Storage**: `src/memory/mod.rs` already uses OmenDB + SQLite.
- **Ingestion**: The agent automatically stores every turn (User message + Assistant reasoning + Tool outcomes) into OmenDB.
- **Query Classification (`needsMemory`)**: A fast-path heuristic to skip vector search for transactional queries (e.g., "/clear", "hello", "what time is it").

### 2.3 Context Assembly Pipeline

For every non-transactional user query:

1.  **Embed**: Generate a vector for the user's message.
2.  **Search**: Query OmenDB for the Top-K (e.g., 50) similar entries.
3.  **Score (ACE + RRF)**:
    - **RRF (Reciprocal Rank Fusion)**: Merge vector similarity with temporal recency.
    - **ACE (Agentic Context Evaluation)**: (Future) Adjust scores based on whether previous "recalled" context helped or hindered the agent.
4.  **Assemble**:
    - Start with a fixed budget (e.g., 10% of context window or 4,000 tokens).
    - Fill with highest-scoring memories until budget is exhausted.
    - Format as a structured "Memory Context" block.

## 3. Integration into Agent Loop

```rust
// src/agent/mod.rs

async fn execute_turn(&self, session: &mut Session, ...) {
    // 1. Memory Recall
    if self.needs_memory(&user_query) {
        let memories = self.memory.recall(&user_query, budget).await?;
        session.inject_context(memories);
    }

    // 2. Stream Response (standard flow)
    let (blocks, tools) = self.stream_response(session, ...).await?;

    // 3. Learning (Ingestion)
    self.memory.learn(session.last_turn()).await?;
}
```

## 4. Proposed File Structure Changes

- `src/memory/embedding.rs`: Embedding provider traits and implementations.
- `src/memory/context.rs`: `ContextAssembler` for ranking and budget-filling.
- `src/memory/classification.rs`: `QueryClassifier` for `needsMemory` logic.

## 5. Metadata Schema (SQLite)

Each entry in OmenDB will link to a SQLite row with:

- `id`: UUID (Primary Key)
- `content`: Raw text (snippet or message)
- `source`: `session_id` or `file_path`
- `type`: `episodic` (chat), `semantic` (code), `procedural` (learned tool usage)
- `timestamp`: Creation date for temporal ranking.

## 6. Differentiators

- **Budget-Aware**: Unlike other agents that dump everything, `ion` stops exactly at the token limit.
- **Bi-Temporal**: Ability to query "What did I know about X last Tuesday?"
- **Local-First**: No external database server required.

## 7. Implementation Phases

1.  **Phase 3.1**: Implement `EmbeddingProvider` (OpenAI/Anthropic).
2.  **Phase 3.2**: Add `learn()` to Agent loop (Automatic Ingestion).
3.  **Phase 3.3**: Implement `recall()` with RRF ranking.
4.  **Phase 3.4**: Integrate `recall()` into `Agent::stream_response`.
