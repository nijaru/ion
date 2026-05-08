# Memory

Memory is deferred during native-loop stabilization. It is not part of the default
P1 model-visible native tool surface.

Ion does not initialize a memory manager on the default native hot path today.
There is no active `/memory` command and no default `recall_memory` or
`remember_memory` tool.

Current boundary:

| Surface | Status |
|---|---|
| `/memory` | Deferred |
| `recall_memory` | Deferred |
| `remember_memory` | Deferred |
| `memory://` namespace | Design direction only |

Future memory should come back through explicit resource namespaces and opt-in
narrow tools, not through default prompt/tool sprawl. Mutation needs approval,
audit, and undo semantics before it becomes model-visible.
