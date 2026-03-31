# Canto Framework (Layer 3) Refactor Plan

The "Rails" for Go-based AI Agents.

## Universal Agent Needs (Core Mandates)

Canto provides the following capabilities to Layer 4 applications (e.g., `ion`):

### 1. Safety by Default (`canto/safety`)
- **Policy Engine:** Gated tool execution based on categories (READ, WRITE, EXECUTE).
- **Mode Management:** Support for READ, EDIT, and YOLO modes at the framework level.
- **Path Isolation:** Built-in RootFS-based sandboxing for file and shell tools.

### 2. Standardized Tooling (`canto/x/tools`)
- **Golden Tools:** Audited, secure versions of `bash`, `file`, `grep`, and `glob`.
- **Built-in Tooling:** Standard implementations for common tasks that all agents need.

### 3. Context Governance (`canto/governor`)
- **Automated Compaction:** Monitoring token usage and triggering soft/hard compaction when limits are approached.
- **Infinite Memory:** Moving old session artifacts to disk-based storage transparently.

### 4. Structured Event Bus
- **Strongly Typed Events:** Replacing generic events with specialized structures.
- **Filtering & Subscription:** Subscribing to specific event types (e.g., `agent.Subscribe(session.ToolEvent)`).

## SOTA Goals (2026)

- **Subagent Spawning:** Native primitives for parent agents to spawn, monitor, and consume child agents.
- **MCP Native Support:** Direct integration with Model Context Protocol servers for tool discovery and execution.
- **Schema-Driven Reasoning:** First-class support for reasoning-aware models (e.g., o1/o3-style chain of thought).
- **Streaming by Default:** Optimized for low-latency terminal interactions.
