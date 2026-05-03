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

## Trust vs Mode

Trust and mode must stay separate:

- trust: persistent user-global decision about whether this checkout can leave
  read-only safety
- mode: per-session approval posture once the workspace is eligible

Mode matrix:

| Workspace trust | `read` | `edit` | `auto` |
|---|---|---|---|
| untrusted | allowed | blocked until `/trust` | blocked until `/trust` |
| trusted | allowed | writes/commands prompt | writes/commands auto-approve |

`/trust` means "allow normal edit/auto behavior in this workspace." It does not
mean auto-approve tools, disable sandboxing, or trust project instructions.

Config should support three postures:

```toml
workspace_trust = "prompt" # prompt | off | strict
```

- `prompt`: unknown workspaces start in `read`; `/trust` enables normal modes
- `off`: no trust gate; personal-machine low-friction behavior
- `strict`: enterprise posture; unknown workspaces stay `read`, with trust
  optionally admin-managed

Shift+Tab should only toggle `read <-> edit`; entering `auto` requires an
explicit slash command or CLI flag.

## Deferred Rollback Work

Visual rollback needs a real checkpoint design, not a weak wrapper around
`git stash` or `git checkout`.

Current checkpoint substrate:

- store: `~/.ion/checkpoints/<checkpoint-id>/`
- manifest: JSON metadata plus per-path entries
- blobs: content-addressed bytes for regular files
- created/untracked files: represented as `absent` before-state entries, so restore removes them
- binary files: stored and restored as bytes with SHA-256 validation
- directories: recorded as directory entries
- first producer: native `write`, `edit`, and `multi_edit` tools create pre-change checkpoints and surface checkpoint IDs in tool results
- no active `/rewind` command; the earlier preview/apply implementation was
  removed from the default TUI path while rollback remains deferred

Still deferred before user-facing rewind:

- command shape and copy for preview/apply
- richer restore conflict behavior when current state diverged for reasons other than the original tool action
- bash/external tool checkpoint coverage
- whether Canto OverlayFS should replace or supplement this path-based durable checkpoint layer

Until those are settled, Ion should treat rewind as an explicit checkpoint
restore feature to redesign later, not hidden command code in the minimal core.

## Sandbox Boundary

Sandbox enforcement is separate from workspace trust and permission mode:

- trust decides the starting permission posture for a checkout
- READ/EDIT/AUTO decides approval behavior
- sandboxing constrains what tool subprocesses can actually touch

Boundary direction:

- local shell execution should sit behind an executor object that owns process
  start, cancellation, process-group cleanup, streaming, and output limits
- Seatbelt, bubblewrap, containers, remote sandboxes, and `just_bash`-style
  executors are implementations of that executor boundary, not separate agent
  loops
- provider credentials are outside the executor environment by default
- tool credentials require explicit secret injection, redaction, and audit
- AUTO mode is only an approval shortcut; it is not a sandbox guarantee

The next sandbox slice should harden bash/external tool execution with real OS
boundaries where available, make the active sandbox visible, and keep AUTO
behavior from feeling safe unless the enforcement layer is actually active.
