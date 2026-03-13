# pi-mono Architecture Research

*Date: 2026-03-13*

## Overview

[pi-mono](https://github.com/badlogic/pi-mono) (also known as **Pi** or **Pi-Agent**) is a minimalist, opinionated AI coding agent created by Mario Zechner. It is the engine behind **OpenClaw** and is known for its extreme performance, context efficiency, and "less is more" philosophy.

## Architecture

Pi is built as a TypeScript monorepo with three strict layers:

1.  **`pi-ai` (Foundation):** A unified LLM abstraction layer. It normalizes protocol differences between Anthropic, OpenAI, Google (Gemini), and local providers (vLLM/Ollama). It manages definitions for over 300 models.
2.  **`pi-agent-core` (State):** Manages `AgentSession` objects. Key features:
    *   **Tree-Based Sessions:** Conversations are stored as branching trees in JSONL format. This allows for `/fork`, `/tree`, and `/backtrack` operations.
    *   **Context Management:** Automatic compaction and model cycling.
3.  **Application Layer:**
    *   **`interactive`:** TUI mode using `pi-tui`. It uses differential rendering to prevent flickering.
    *   **`print`:** CLI output.
    *   **`rpc`:** JSON-RPC for programmatic access.

## Key Patterns for Ion

### 1. The 1,000 Token Rule
Pi keeps its system prompt and tool definitions under 1,000 tokens. This maximizes the context available for the project. Competing agents often use 3,000-5,000 tokens for "features" (planning, multi-step thoughts) that could be handled as tools or instructions in the project root.

**Takeaway:** We should resist bloat in our base `ion` system prompt. Move complexity into tools or "skills."

### 2. "Skills" and On-Demand Documentation
Instead of injecting all tool definitions into every prompt, Pi can use "skills."
- A skill includes a README.
- The agent only reads the documentation for a skill when it decides it needs it (via a `use_skill` tool or similar).

**Takeaway:** This is a great way to handle specialized workflows (like Docker, Kubernetes, or specific languages) without polluting the global context.

### 3. Hierarchical Context (AGENTS.md)
Pi loads instructions from:
1. `~/.pi/agent/` (Global)
2. Parent directories (Organizational)
3. Project root (Local)

**Takeaway:** We are already following this pattern with `AGENTS.md`. We should ensure our `AgentSession` properly merges these.

### 4. Tree-Based History
By storing history as a tree instead of a flat list, Pi allows users to:
- Explore multiple solution paths for a single bug.
- Backtrack to a "stable" state without losing the failed branch's history.

**Takeaway:** Our `AgentSession` interface should eventually support branching. This is especially powerful for "YOLO" autonomous modes.

### 5. Custom TUI vs Frameworks
Pi uses a custom `pi-tui` library designed for high-performance terminal rendering. It avoids flickering by using synchronous escape sequences and a virtual terminal buffer.

**Takeaway:** Our choice of **Bubble Tea v2** is well-aligned here, as it also prioritizes performance and robust TUI primitives.

## Comparison with autoresearch

| Feature | autoresearch | pi-mono |
| --- | --- | --- |
| **Focus** | Autonomous RLM Loops | Interactive Coding Assistant |
| **State** | Flat loop (Keep/Discard) | Branching Tree (Fork/Backtrack) |
| **Philosophy** | Direct Objective Metric | Minimal System Prompt |
| **Memory** | Markdown files | JSONL Tree + Markdown |

## Next Steps for Ion

1.  **Simplify `NativeIonSession`:** Keep the initial system prompt lean.
2.  **Tree-Based Storage:** Evaluate if our SQLite/Persistence layer (`tk-vmdl`) should support a parent-child relationship for turns to enable branching.
3.  **Provider Layer:** Look at how `pi-ai` normalizes tool calling across Gemini and Anthropic (this is a common pain point).
