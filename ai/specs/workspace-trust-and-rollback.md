# Workspace Trust And Rollback

## Current Slice

Ion now has persistent user-global workspace trust state:

- storage: `~/.ion/trusted_workspaces.json`
- key: absolute workspace path
- command: `/trust`
- status command: `/trust status`

Startup behavior:

- trusted workspace: use the requested/configured startup mode
- untrusted workspace: force READ mode and print a startup notice

This makes trust state a user decision, not a project-controlled config. Project
files cannot mark themselves trusted.

## Deferred Rollback Work

Visual rollback needs a real checkpoint design, not a weak wrapper around
`git stash` or `git checkout`. Required decisions before implementation:

- checkpoint format: Canto VFS/CoW vs git-backed snapshot
- untracked file handling
- binary file handling
- restore conflict behavior
- audit event emitted before and after restore
- TUI confirmation shape for destructive restore

Until those are settled, Ion should expose trust UX but not promise rewind.
