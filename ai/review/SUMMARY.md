# Codebase Review Summary

**Last Updated:** 2026-01-25
**Status:** Historical - see tui-analysis-2026-02-04.md for latest TUI review

## Overall Health

| Module    | Health | Notes                          |
| --------- | ------ | ------------------------------ |
| tui/      | GOOD   | Well-structured, 6 submodules  |
| agent/    | GOOD   | Clean turn loop, plan support  |
| provider/ | GOOD   | Multi-provider abstraction     |
| tool/     | GOOD   | Orchestrator + approval system |
| session/  | GOOD   | SQLite persistence             |
| skill/    | GOOD   | YAML frontmatter, lazy loading |
| mcp/      | OK     | Needs tests, cleanup deferred  |

## Issues - All Resolved

### Agent Module

- [x] **execute_tools_parallel unwrap** - Fixed: proper `Option::collect` with error
- [x] **Template unwraps** - Fixed: using `expect()` with descriptive messages
- [x] **Greedy JSON regex** - Fixed: non-greedy `\{.*?\}`
- [x] **Message queue poisoning** - Fixed: `poisoned.into_inner()` recovery
- [x] **Plan never cleared** - Fixed: `Agent::clear_plan()` on `/clear`

### TUI Module

- [x] **Token overflow risk** - Fixed: `saturating_mul(100)`
- [x] **History index not reset** - Fixed: reset in all slash command branches

## Deferred (Low Priority - Not Bugs)

- MCP process cleanup (requires significant refactor)
- Extract retry logic to helper (code duplication, not bug)
- Compaction post-validation (pruning logic is correct)
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
  └── builtins/       - read, write, edit, bash, glob, grep, web_fetch
```

## Code Quality

**File sizes (largest):**
| File | Lines | Status |
|------|-------|--------|
| composer/mod.rs | 1082 | OK - cohesive component |
| render.rs | 800 | OK - single concern |
| agent/mod.rs | 752 | OK - main loop |
| registry.rs | 695 | OK - model registry |

**No splitting recommended** - files are large but cohesive with single responsibilities.

## Future Refactoring Opportunities

1. Extract retry logic to helper (reduces ~50 lines duplication)
2. Group pickers into `tui/pickers/` subdirectory
3. Add integration tests for tool builtins
