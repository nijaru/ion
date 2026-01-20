# Session Context (2026-01-16)

## Just Completed

### Architecture Design - Sub-Agents vs Skills

Finalized architecture based on research (Pi-Mono, Claude Code, Cognition, UBC papers):

| Component    | Type      | Model | Purpose                              |
| ------------ | --------- | ----- | ------------------------------------ |
| `explorer`   | Sub-agent | Fast  | File search, pattern matching        |
| `researcher` | Sub-agent | Full  | Web search, doc synthesis            |
| `reviewer`   | Sub-agent | Full  | Build, test, code analysis           |
| `developer`  | Skill     | -     | Code implementation (same context)   |
| `designer`   | Skill     | -     | Architecture planning (same context) |
| `refactor`   | Skill     | -     | Code restructuring (same context)    |

**Key principle**: Sub-agents for context isolation (large → small), skills for behavior modification (same context).

### Session Persistence (tk-ay6c) - Complete

SQLite-backed with TUI integration:

**SessionStore API:**

```rust
SessionStore::open(path) -> Result<Self>
SessionStore::save(&session) -> Result<()>      // Incremental append
SessionStore::load(id) -> Result<Session>
SessionStore::list_recent(limit) -> Result<Vec<SessionSummary>>
SessionStore::delete(id) -> Result<()>
```

### Scroll Support (tk-uagv) - Complete

Vim-style scrolling: j/k, Ctrl+d/u, g/G

## Current State

- **Phase 4 (TUI Polish)**: In progress
- **Tests**: 16 passing
- **Clippy**: Clean

## Next Tasks

### Critical (Core Architecture)

```
- Context compaction (prevents overflow)
- Memory querying (differentiator, currently unused)
- Skills loader (SKILL.md parsing)
```

### TUI Polish

```
tk-7j11  - Implement TUI markdown rendering
tk-atdh  - Implement TUI diff view
```

**Priority**: Context compaction and memory querying are critical gaps.

## Key Architecture Notes

**Sub-agents vs Skills:**

- Sub-agents isolate context for expansion tasks (explorer, researcher, reviewer)
- Skills modify behavior in main context (developer, designer, refactor)
- Binary model selection: Fast (explorer) or Full (inherit)

**Memory system (unused differentiator):**

- Stores to OmenDB + SQLite, but never queries
- Needs: `search()`, `recall()`, `compact()` for context management

**Session flow:**

```
User input -> Agent::run_task() -> StreamEvents -> session_rx -> App
                                                              -> SessionStore::save()
```

## Files to Read

If resuming work:

1. `ai/STATUS.md` - Current project status
2. `ai/design/sub-agents.md` - Sub-agents vs skills architecture
3. `ai/DECISIONS.md` - Recent architecture decisions
4. `src/memory/mod.rs` - Memory system (needs querying)
