# Design Spec: Context & Memory

## 1. Overview

The Context & Memory system manages the lifecycle of information in `ion`, from active conversation to long-term vector storage.

## 2. Layers

### 2.1 Persistence (SQLite)

- **Store**: `SessionStore` handles message history and session metadata.
- **Schema**:
  - `sessions`: id, model, working_dir, created_at
  - `messages`: session_id, role, content (JSON), tokens, timestamp
- **API**: `load(id)`, `save(session)`, `list_recent(limit)`

### 2.2 Short-Term: Context Compaction

- **Goal**: Maintain model performance by keeping context < 55% of the window.
- **Mechanism**: Tiered pruning followed by structured summarization.
  - **Tier 1**: Truncate large tool outputs (>2k) to head/tail.
  - **Tier 2**: Remove verbatim tool outputs, keep reference.
  - **Tier 3**: Summarize oldest messages using a fast model.
- **Principles**: Always preserve **Failed Approaches** and **Decisions**.

### 2.3 Long-Term: Memory Retrieval (OmenDB)

- **Goal**: Budget-aware recall of cross-session information.
- **Pipeline**:
  1. **Embed**: Generate query vector (Provider or Local).
  2. **Search**: Top-K search in OmenDB.
  3. **Rank (RRF)**: Merge Vector Similarity with Temporal Recency.
  4. **Assemble**: Fill a fixed budget (e.g., 4k tokens) with highest-scoring snippets.
- **Integration**: Injected into system prompt or as a dedicated `Context` message before `stream_response`.

## 3. Implementation

- `src/session/`: Persistence logic.
- `src/compaction/`: Token counting and pruning logic.
- `src/memory/`: OmenDB integration, embedding, and ranking.

## 4. Key Decisions

- **Bi-temporal**: OmenDB allows point-in-time queries ("What did I know then?").
- **ACE Scoring**: Adjust relevance based on previous agent helpfulness.
- **Native OmenDB**: No IPC overhead; Rust-native vector search.
