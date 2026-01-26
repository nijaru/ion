# Codebase Review Summary

**Last Updated:** 2026-01-25
**Status:** Sprint 8 fixes applied

## Overall Health

| Module    | Health | Notes                          |
| --------- | ------ | ------------------------------ |
| tui/      | GOOD   | Well-structured, 6 submodules  |
| agent/    | GOOD   | Clean turn loop, plan support  |
| provider/ | GOOD   | Multi-provider abstraction     |
| tool/     | GOOD   | Orchestrator + approval system |
| session/  | GOOD   | SQLite persistence             |
| mcp/      | OK     | Needs tests, cleanup deferred  |

## Sprint 8 Fixes Applied

1. **Greedy JSON regex** - `designer.rs:10` - Changed `\{.*\}` to `\{.*?\}` (non-greedy)
2. **Message queue silent drop** - `events.rs:188-192` - Now recovers from poisoned lock with warning
3. **Session reload incomplete** - `session.rs:484-530` - Now shows tool calls and results
4. **Plan never cleared** - Added `Agent::clear_plan()`, called on `/clear`

## Deferred (Low Priority)

- MCP process cleanup (requires significant refactor)
- Extract duplicated filter logic in registry.rs (refactor, not bug)
- Config merge edge case

## Architecture

**Layers:**

```
CLI (main.rs)
  ↓
TUI (tui/)
  ├── events.rs   - Event dispatch
  ├── session.rs  - State management
  ├── render.rs   - Drawing
  ├── input.rs    - Composer integration
  ├── types.rs    - Enums, constants
  └── util.rs     - Helpers
  ↓
Agent (agent/)
  ├── mod.rs      - Turn loop, tool execution
  ├── context.rs  - System prompt assembly
  ├── designer.rs - Plan generation
  └── instructions.rs - AGENTS.md loader
  ↓
Provider (provider/)
  ├── client.rs   - LLM API trait
  ├── registry.rs - Model fetching/caching
  └── types.rs    - Message/ContentBlock types
  ↓
Tool (tool/)
  ├── orchestrator.rs - Tool dispatch, approval
  └── builtins/       - read, write, edit, bash, glob, grep
```

**Data Flow:**

1. User input → TUI events → Agent::run_task
2. Agent loop: stream_response → execute_tools_parallel → repeat until no tools
3. Tool results → TUI message_list → render

**Key Patterns:**

- Arc<dyn Trait> for provider abstraction
- mpsc channels for async events (agent → TUI)
- CancellationToken for abort handling
- Mutex/RwLock for shared state (with poison recovery)

## Code Organization Assessment

**Current structure is appropriate.** No major reorganization needed.

**Strengths:**

- Clear module boundaries
- Single responsibility per file
- Consistent error handling (anyhow/thiserror)
- Good separation: TUI ↔ Agent ↔ Provider

**Minor improvements (optional):**

- `tui/` could group pickers into `tui/pickers/` (model, provider, session)
- `tool/builtins/` could have integration tests
