# Workspace Trust

Ion stores trusted workspaces in:

```text
~/.ion/trusted_workspaces.json
```

When a workspace has not been trusted, Ion starts in READ mode even if the
configured default mode is EDIT or YOLO. This keeps first contact with an
unknown checkout look-only until the user explicitly trusts it.

Commands:

| Command | Behavior |
|---|---|
| `/trust` | Mark the current workspace trusted |
| `/trust status` | Show whether the current workspace is trusted |

Trust state is keyed by the absolute workspace path. It is user-global, not
project-local, so a repository cannot mark itself trusted by editing files.

Visual rewind/checkpoint support is not implemented by this trust file. That
needs a separate checkpoint format with explicit restore semantics.
