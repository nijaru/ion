# Memory

Memory is deferred during native-loop stabilization. It is not part of the default
P1 model-visible native tool surface.

Ion uses Canto memory for workspace-scoped recall.

Commands:

| Command | Behavior |
|---|---|
| `/memory` | Show the workspace memory index tree |
| `/memory <query>` | Search workspace memory |

Agent tools:

| Tool | Behavior |
|---|---|
| `recall_memory` | Search workspace memory from the agent loop |
| `remember_memory` | Store a semantic workspace memory |

This is the first visible memory UX. Wiki compilation and richer collection
management stay separate from the core loop until the read/search/store path is
boring and reliable.
