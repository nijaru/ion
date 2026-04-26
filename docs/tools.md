# Tools

Ion uses Canto's lazy tool loading. When the registered tool surface is larger
than the lazy threshold, the model initially sees `search_tools` plus any eager
core tools. It can call `search_tools` to unlock specific hidden tool schemas.

Use:

```text
/tools
```

to show the registered tool count, whether lazy loading is active, and the
current tool names. The startup banner and `/tools` both report the active bash
sandbox posture for the native Canto backend.

Bash sandboxing is configured with:

```text
ION_SANDBOX=off|auto|seatbelt|bubblewrap
```

Explicit `seatbelt` and `bubblewrap` modes fail closed when their backend is
unavailable. `auto` uses the platform backend when present and reports when it
falls back to `off`.

Approval tiers remain deliberately small:

| Mode | Behavior |
|---|---|
| READ | read tools allowed; write/execute blocked; sensitive asks |
| EDIT | read tools allowed; write/execute/sensitive follow policy |
| YOLO | all tools allowed |

Granular persistent rules live in `~/.ion/policy.yaml`; see
`docs/security/policy.md`.

Native `write`, `edit`, and `multi_edit` create pre-change checkpoints before
they mutate files. Tool results include the checkpoint ID, which can be previewed
with `/rewind <checkpoint-id>` and restored with `/rewind <checkpoint-id>
--confirm`.
