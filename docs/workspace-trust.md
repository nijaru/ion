# Workspace Trust

Ion stores trusted workspaces in:

```text
~/.ion/trusted_workspaces.json
```

When a workspace has not been trusted, Ion starts in READ mode even if the
configured default mode is EDIT or AUTO. This keeps first contact with an
unknown checkout look-only until the user explicitly trusts it.

Trust and mode are separate:

| Concept | Meaning |
|---|---|
| Trust | Whether this workspace may leave read-only safety |
| Mode | Whether Ion asks before writes/commands once the workspace is trusted |

Mode behavior:

| Workspace | READ | EDIT | AUTO |
|---|---|---|---|
| untrusted | allowed | blocked until `/trust` | blocked until `/trust` |
| trusted | allowed | asks before writes/commands | runs writes/commands without prompts |

Commands:

| Command | Behavior |
|---|---|
| `/trust` | Mark the current workspace trusted |
| `/trust status` | Show whether the current workspace is trusted |
| `/rewind <checkpoint-id>` | Preview the paths a checkpoint restore would change |
| `/rewind <checkpoint-id> --confirm` | Restore the checkpoint after explicit confirmation |

`/trust` does not auto-approve tools or disable sandboxing. It only allows the
workspace to use normal EDIT/AUTO modes.

Trust state is keyed by the absolute workspace path. It is user-global, not
project-local, so a repository cannot mark itself trusted by editing files.

Checkpoint state is separate from trust state and lives under:

```text
~/.ion/checkpoints
```

Native file edits create pre-change checkpoints. Rewind is intentionally
two-step: preview first, restore only with `--confirm`.
