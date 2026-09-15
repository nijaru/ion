# Working on Ion

## Direction and ownership

- `ARCHITECTURE.md` owns the accepted turn-engine contracts. `README.md` describes
  what currently works. Target, implemented and validated are different states.
- The 2026-09-15 design replaces the former generic task runtime. Current source is
  evidence, not a compatibility constraint. Do not deepen TaskPlan, task graphs or
  the resident state/undo-journal design, or add Turn as a second authority over them.
- Replace obsolete production paths directly. Ion is unreleased v0: no compatibility
  shims, parallel runtimes or unused public surfaces kept for hypothetical consumers.
  Git preserves old code and documents; retain useful failure scenarios as new tests.
- Keep the product a Pi-like terminal coding agent with the same headless/library
  path. Workers are optional and follow a measured single-agent baseline. Memory,
  gateways, schedules and general workflow authoring are outside current scope.
- Keep provider contracts independent of sessions/storage/TUI. Give each module one
  semantic owner; avoid generic manager/helper buckets and crate-per-noun scaffolding.

## Changes

- Before a slice, identify observable behavior, semantic owner, failure/recovery
  boundary and acceptance test. Read affected code and current Git status first.
- Decide consequential boundaries before implementing them. Update the architecture
  when evidence changes a contract; do not conceal a disagreement with an adapter.
  Keep research, working rationale and cutover tracking with their knowledge owner.
- Add the boundary regression before marking a defect repaired. Preserve uncertainty,
  cancellation fencing, durable admission and bounded resources when deleting APIs.
- Do not equate declared capabilities with confinement, future cancellation with stopped
  external effects, or green scripted tests with a working live coding agent.
- Keep this as the only repository agent-instruction file. Add a project skill only
  for a demonstrated recurring workflow; do not recreate design/research directories
  as agent context. Public documentation must remain self-contained.

## Validation

Use the checked-in Rust 1.98.0 toolchain and run:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

Run targeted tests during iteration. Crash, cancellation, storage and provider changes
need deterministic fault tests; overflow-sensitive changes also need release checks.
Terminal changes need reducer/PTY checks and real-terminal smoke, not only golden frames.
The current `scripts/smoke.sh` targets the excluded legacy application and is not a
working fresh-workspace gate; replace it when the executable returns, not with a shim.
For documentation-only work, verify links, authority/status consistency and preservation;
do not claim runtime or live-model validation that was not performed.
