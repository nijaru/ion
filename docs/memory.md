# Memory

Ion has an opt-in, workspace-scoped note store. It is deliberately separate from
session persistence: notes never become session-tree entries, are never injected
into prompts automatically, and are returned to the model as untrusted data.

The explicit host command is available in the TUI:

```text
/memory
/memory search <query>
/memory all
/memory forget <memory-id>
/memory restore <memory-id>
```

`/memory` uses the current workspace and stores notes in
`~/.ion/data/memory.db`. Deletion is a soft delete. The store retains an audit
record for add, delete, and restore operations, and `all` shows deleted notes so
they can be restored.

Model-visible memory tools are opt-in through `~/.ion/config.toml`:

```toml
memory_tools = "on"
```

This adds `recall_memory` and `remember_memory` to the runtime registry. Recall
requires an explicit literal query. Remember writes only after the normal Ion
approval boundary, even when other local tools use trusted mode. Memory tool
activation follows `tool_mode`: read mode exposes recall only, while coding and
all modes expose both tools.

Memory is intentionally not a semantic index, automatic context manager,
`memory://` protocol, or Pi-compatible file format. The contract is a small,
auditable Ion service for explicit workspace notes.
