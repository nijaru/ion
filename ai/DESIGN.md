# ion System Design

## Overview

`ion` is a high-performance Rust-based terminal agent designed for deep codebase understanding and autonomous task execution. It distinguishes itself through a "budget-aware memory context assembly" system, using native Rust vector search (OmenDB) to build highly relevant context windows.

**Runtime**: Rust (Stable)
**Distribution**: Single static binary

```
User Request → ion CLI → Agent Core → OmenDB Memory → Tool Execution → Response
                                   ↓
                            Learn from outcome
```

## Architecture Layers

### 1. TUI Layer (`ratatui`)

Professional terminal interface with high-readability colors (Catppuccin Mocha).

- **Chat Buffer**: Scrollable history with thin-line delimiters.
- **Diff View**: Inline unified diffs using the `similar` crate.
- **Statusline**: Customizable `{Model} · {Context}% | {Branch} · {Cwd}`.

### 2. Provider Layer

Multi-provider abstraction via `llm` crate supporting:

- **Anthropic**: Direct Claude API
- **Google**: Gemini via AI Studio
- **Groq**: Fast inference
- **Ollama**: Local models
- **OpenAI**: Direct GPT API
- **OpenRouter**: 200+ models aggregator

### 3. Agent Layer

The core multi-turn loop is designed for high performance and observability:

- **Designer Sub-agent**: For complex tasks, a specialized planning sub-agent decomposes the request into a structured JSON task graph. This ensures the main agent follows a logical sequence with clear dependencies.

- **Decomposed Phases**: Turn logic is split into `stream_response` and `execute_tools` phases.

- **Parallel Execution**: Multiple tool calls in a single turn are executed concurrently using `tokio::task::JoinSet`.

- **Zero-Copy Context**: Message history and tool definitions are wrapped in `Arc` to avoid expensive cloning during context assembly and provider requests.

## Memory Layer (The OmenDB Advantage)

Native Rust integration with OmenDB for:

- **Tree-sitter Integration**: Precise symbol mapping (functions, classes, structs) using language-aware grammars instead of regex. This enables high-fidelity codebase navigation.

- **Agentic Filesystem Memory**: Inspired by Letta/MemGPT research, `ion` treats the local filesystem as a primary memory tool.

Agents are encouraged to use `grep` and `glob` iteratively, leveraging the fact that LLMs are heavily trained on filesystem-style information retrieval.

- **Lazy Indexing**: Symbols (functions, classes) and file metadata are only indexed when the agent interacts with a file (Read/Write). This prevents O(N) startup bottlenecks in large repositories.
- **Background Indexing Worker**: A dedicated `IndexingWorker` manages a queue of embedding/storage requests, preventing UI lag during ingestion.
- **Hybrid Retrieval (RAG)**: Combines Vector similarity and BM25 full-text search using **Reciprocal Rank Fusion (RRF)** for unstructured context.
- **Advanced Scoring (ACE)**: Boosts "helpful" memories and penalizes "harmful" ones based on agent self-correction patterns.
- **Time Decay**: Type-specific half-lives (Semantic=7d, Episodic=24h, Working=1h) ensure relevant history is prioritized.
- **Batch Retrieval**: SQLite lookups are batched (`IN` clause) after vector search to avoid N+1 queries.
- **Lazy Persistence**: Disk flushing is decoupled from indexing for high-throughput background ingestion.
- **Multi-Vector Support**: Simulated ColBERT MaxSim retrieval (`Σ max(q_i · d_j)`) for token-level semantic granularity.
- **Hardware Optimized Inference**: Local embeddings use `ort` (ONNX) with platform-specific acceleration:
  - **macOS**: CoreML (Apple Silicon Neural Engine)
  - **Linux**: CUDA (NVIDIA) or ROCm (AMD)
  - **Windows**: DirectML (Universal GPU acceleration)

## TUI Layer (High-Performance Interaction)

- **Grapheme-Based Input**: Uses `unicode-segmentation` for robust cursor movement and editing of wide characters (CJK, Emoji).
- **Markdown Caching**: Formatted segments are cached in `MessageEntry` to ensure 60fps rendering without re-allocation.
- **Async Communication**: `tokio::sync::mpsc` channels decouple the agent's logic from the UI frame loop.

## Data Persistence

```
~/.config/ion/
├── config.toml          # Global settings
├── models.toml          # Model preferences
└── keys.toml            # Encrypted or separate keys

~/.local/share/ion/
├── sessions/            # Persisted message history (SQLite)
└── memory/              # OmenDB vector indices
```

## Tool Framework

- **Built-in**: `read`, `write`, `grep`, `glob`, `bash`.
- **Formatting**: Tool execution is logged minimally; file edits show standard git-style diffs.
- **Safety**: 3-mode permission matrix (Read, Write, AGI) with interactive `y/n/a/A/s` prompts in Write mode.

## Agent Loop Decomposition

To improve reliability and fix "silent hang" bugs, the Agent loop is being refactored into discrete phases:

1.  **Response Phase (`stream_response`)**: Handles provider streaming, collects deltas, and extracts tool calls.
2.  **Tool Phase (`execute_turn_tools`)**: Sequentially (or concurrently where safe) executes turn-based tool calls.
3.  **State Phase (`update_session`)**: Commits assistant and tool turns to history.

This separation allows for robust error handling at each boundary and enables unit testing of tool execution without requiring a live LLM.

## TUI: MessageList Component

The TUI state is being decoupled by extracting message history into a `MessageList` struct.

- **Responsibilities**:
  - Accumulating text and thinking deltas.
  - Formatting tool execution status and results.
  - Handling future scrollback, search, and markdown rendering.
- **Interface**: `MessageList::push_event(AgentEvent)`

## Design Philosophy

- **Minimalist**: Focus on the code and the chat; avoid UI clutter.
- **Native**: Leverage Rust's speed for instant tool feedback and search.
- **Invisible Intelligence**: Memory works in the background to improve answers without requiring user management.
