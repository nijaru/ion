# Claude Code Architecture Research

*Date: 2026-03-13*

## Overview
[Claude Code](https://github.com/anthropic/claude-code) is Anthropic's official agentic CLI. It is a high-performance terminal agent designed for deep codebase integration and autonomous problem-solving.

## Architecture & UX
- **Gather-Act-Verify Loop:** Operates in a three-phase cycle that can be interrupted by the user at any point.
- **Programmatic Tool Calling:** Orchestrates multiple tools in parallel (e.g., searching and reading files simultaneously), which reduces context bloat by shielding the main LLM from intermediate raw data.
- **React/Ink TUI:** Built with TypeScript, React, Ink, and Yoga for a responsive, modern terminal UI.
- **Modes:** Supports explicit modes like `Plan` (no execution), `Auto-accept` (unattended), and `Default` (permission gated).

## Memory & Context Management
- **Local Persistence:** Sessions, tool usage, and file snapshots are stored locally, enabling `/rewind`, `/restore`, and forking worktrees.
- **Automatic Compaction:** As context window limits are approached, Claude automatically compacts older history while preserving key decisions and metadata.
- **Lazy-Loaded Skills:** Tools and "skills" are loaded on-demand to minimize the initial prompt size.

## Best-in-Class Takeaways for Ion
1. **Parallel Tool Orchestration:** Don't just call one tool at a time. Let the agent request multiple actions and return a synthesized result to keep the main context lean.
2. **Reversible Checkpoints:** The ability to `/rewind` or `/fork` a session is critical for developers exploring different architectural paths.
3. **Context Compaction:** Automating the "pruning" of old logs while keeping the "decisions" is a superior pattern to simple truncation.
4. **Explicit Permission Modes:** Allow users to toggle between "YOLO" (auto-accept) and "Gated" modes easily via TUI shortcuts.
