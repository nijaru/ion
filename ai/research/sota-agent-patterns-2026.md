# SOTA Agent & RLM Patterns 2026

*Date: 2026-03-13*

## Overview
Based on a synthesis of Claude Code, pi-mono, autoresearch, Crush, Droid, and Letta, we have identified the current State-of-the-Art (SOTA) patterns for high-performance TUI agents in 2026.

## 1. Context & Memory Management
- **The 1,000 Token Rule:** Minimalist system prompts and tool definitions (<1,000 tokens) are now industry standard for maximizing usable project context.
- **Hierarchical Context Persistence:** Instructions should be merged from global (`~/.config/`), organizational (parent dirs), and local (project root `AGENTS.md`) sources.
- **Automatic Compaction & Summarization:** Pruning old logs while recursively summarizing key decisions is the superior alternative to simple context truncation.
- **Git-Based Versioning for State:** Versioning the agent's internal state (decisions, memory, and forked worktrees) provides a "Time Travel" debugging experience for AI agents.

## 2. Agent Orchestration & Reasoning
- **Gather-Act-Verify Cycles:** Autonomous agents should operate in explicit phases: research/gather info → act/execute → verify/check success.
- **Parallel Tool Orchestration:** Agents that can generate code to call several tools concurrently (e.g., searching, reading, and running tests at once) significantly reduce turn latency and context bloat.
- **Role Specialization (Guidance vs. Execution):** Splitting agents into a **Guidance Agent** (maintains long-term memory/planning) and an **Execution Agent** (grinds on the code) prevents the system from getting stuck in local optima.
- **Sleep-Time Asynchronous Refinement:** Using idle time to synthesize logs, update documentation (`STATUS.md`), and refine skills improves future performance without user friction.

## 3. Reasoning Loops & "Grounding"
- **Inline Planning at the Context Tail:** Keeping a concise, updated plan at the end of the context window is more effective for coherence than a separate state or long system prompt.
- **LSP-Driven Grounding:** Feeding real-time diagnostics (syntax errors, type errors) into the agent's context allows it to self-correct during the "Verify" phase of its loop.
- **Time-Aware Tool Choice:** Annotating tool runtimes allows the agent to make smarter, more cost-effective decisions (e.g., "should I run an exhaustive grep or a scoped search?").

## 4. TUI & UX Patterns
- **Differential Rendering & Synchronous Escapes:** High-performance TUIs avoid flickering by using efficient rendering primitives (like Bubble Tea or custom vterm buffers).
- **Checkpoints & Worktree Forking:** Developers should be able to `/fork` a conversation and explore two different architectural paths in parallel terminal sessions.
- **Explicit Permission Shortcuts:** TUI shortcuts (e.g., `Shift+Tab`) should allow users to toggle between "Auto-accept" (YOLO) and "Gated" modes mid-session.

## Best-in-Class Takeaways for Ion
1. **Durable File-Based Memory:** Use `AGENTS.md` and `STATUS.md` as the primary grounding source for agents.
2. **Asynchronous Guidance Agent:** Implement a background "Guidance" sub-agent to keep project docs updated.
3. **Programmatic Parallel Tooling:** Design our tool-call layer to handle batches of operations.
4. **Tree-Based Session Storage:** Transition our SQLite schema to support branching and forking sessions.
5. **Context Efficiency First:** Keep the core system prompt under 1,000 tokens.
