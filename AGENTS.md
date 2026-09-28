# Working on Ion

## Direction

- `ARCHITECTURE.md` holds the current coding-agent design target;
  `README.md` describes implemented and validated behavior.
- Build one Pi-like local coding loop for TUI, headless and library hosts:
  read, edit, write, native shell, project instructions, model catalog,
  automatic environment keys, optional masked API-key entry and resumable
  sessions. OAuth is not a first-use gate.
- Ion is unreleased v0. Replace obsolete R1 representations directly. Do
  not keep a generic task graph, second runtime, private importer/registry,
  attempt ledger or compatibility facade simply because it existed before.
  Retain a mechanism only when the coding contract or a reproduced failure
  warrants it.
- Tools act in the live working directory with host permissions. There is
  no implicit sandbox or VM. Report cancellation and external effects only
  as observed; never silently rerun an incomplete tool call on reopen.
- Keep provider transports and credentials independent of Session storage
  and terminal rendering. Give each rule one semantic owner.
- Workers, personal memory, gateways, schedules and workflow authoring are
  outside the first usable coding-agent scope.

## Changes

- Before a substantial coding-path slice, trace affected Ion code and Git
  status, then inspect analogous current Pi source, tests and recent fixes at
  a recorded revision. Check other harnesses when the design choice needs
  them. Start from the user workflow and Ion's semantic owner: a reference
  difference alone is not a requirement. Reconcile the accepted contract
  before changing implementation, and verify the failure boundary.
- Preserve user work and secrets. Delete obsolete production paths once the
  replacement owns the behavior; Git retains historical source. Update
  public documentation when behavior or a maintainer contract changes.
- Test the actual headless and terminal surfaces for changes that affect
  them. Scripted provider tests do not establish live coding ability.
- Keep this the only repository agent-instruction file. Research, decisions
  and rewrite tracking belong in the knowledge repository rather than a
  new documentation scaffold here.

## Validation

Use the checked-in Rust 1.98.0 toolchain:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

Run targeted tests during implementation and `scripts/smoke.sh` for the
headless offline submit/reopen path. Add focused fault tests for changed
storage, provider or cancellation behavior. Terminal changes need PTY and
real-terminal checks. Re-run relevant gates after the last code edit. For
documentation-only work, verify links, authority and status consistency;
do not claim runtime validation.
