# Letta (MemGPT) Memory Research

*Date: 2026-03-13*

## Overview
[Letta](https://www.letta.com/) (formerly MemGPT) is a "memory-first" platform for building stateful AI agents. It focuses on solving the context window limitation through a hierarchical memory model.

## Memory Architecture
- **In-Context Core Memory (RAM):** Editable, pinned blocks of context (e.g., persona, user info, task objectives) with character limits. The agent can update these via tools.
- **Archival & Recall Memory (Disk):** Externally stored, searchable history that the agent can "retrieve" when needed, creating an illusion of unlimited memory.
- **Sleep-Time Agents:** Asynchronous sub-agents that operate during idle periods to reorganize memory, distill insights, and refine context without blocking the main interaction loop.
- **Perpetual Thread:** Agents maintain a single, infinite-feeling conversation thread that can be ported across models or providers.

## Context Management
- **Intelligent Eviction:** Uses recursive summarization and eviction strategies to manage context window pressure when limits are hit.
- **Context Repositories:** Git-based versioning and diffing of agent state, allowing for robust rollbacks and audit trails.

## Best-in-Class Takeaways for Ion
1. **Sleep-Time Background Refinement:** Use background sub-agents to asynchronously update `STATUS.md` or `DESIGN.md` while the user is away.
2. **Pinned Memory Blocks:** Allow the agent to "pin" certain files or instructions to its core context (like our `AGENTS.md` and `STATUS.md` system).
3. **Recursive Summarization:** When truncating history, summarize it recursively to preserve the "gist" of the conversation.
4. **Git-Style State Diffs:** Version the agent's internal state (memory, plan, and decisions) just like we version code.
