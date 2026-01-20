# Design Specs

**Purpose**: Consolidated architectural specifications for `ion`.

## Core Pillars

| Spec                             | Purpose                                       | Status  |
| :------------------------------- | :-------------------------------------------- | :------ |
| [Context & Memory](./context.md) | Persistence, Compaction, and OmenDB Retrieval | Current |
| [Agent & Tools](./agent.md)      | Multi-turn loop, Sub-agents, and Skills       | Current |
| [TUI Interface](./tui.md)        | Layout, Interaction, and Visual Polish        | Current |

## Reference Specs (Legacy)

These are archived in `ai/design/_archive/` for historical context.

- `rust-architecture.md`: Initial Rust transition notes.
- `sub-agents.md`: Detailed sub-agent vs skill research.
- `context-compaction.md`: Original token budget research.
- `hybrid-search.md`: Early ripgrep+chroma design.
- `tui-spec.md`: Initial keybinding layout.
- `session-persistence-schema.md`: Initial SQLite schema.
