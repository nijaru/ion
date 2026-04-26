# Tools

Ion uses Canto's lazy tool loading. When the registered tool surface is larger
than the lazy threshold, the model initially sees `search_tools` plus any eager
core tools. It can call `search_tools` to unlock specific hidden tool schemas.

Use:

```text
/tools
```

to show the registered tool count, whether lazy loading is active, and the
current tool names.

Approval tiers remain deliberately small:

| Mode | Behavior |
|---|---|
| READ | read tools allowed; write/execute blocked; sensitive asks |
| EDIT | read tools allowed; write/execute/sensitive follow policy |
| YOLO | all tools allowed |

Granular persistent rules live in `~/.ion/policy.yaml`; see
`docs/security/policy.md`.
