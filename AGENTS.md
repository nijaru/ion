# Working on Ion

## Direction and ownership

- `ARCHITECTURE.md` owns the accepted coding-agent contracts. `README.md` describes
  what currently works. Target, implemented and validated are different states.
- The maintained Turn/Session engine owns one coding loop for TUI and headless
  clients. Preserve truthful attempt evidence and avoid replaying uncertain
  effects, but do not require a VM, sandbox, protected workspace registry or
  all-descendant quiescence for ordinary host commands. Local shell execution
  uses the live checkout and native toolchains on macOS and Linux; cancellation
  is best effort and must be described as such. Do not reintroduce a generic
  task/plan graph, resident semantic mirror or undo journal.
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
  Keep research, working rationale and rewrite tracking with their knowledge owner.
- Add the boundary regression before marking a defect repaired. During v0 work,
  test the new owner/invariant first rather than patching superseded modules.
  Rewrite checkpoints may be staged as commits for review, but no checkpoint is a
  compatibility/migration layer and no old+new production runtime may coexist.
  Preserve uncertainty, no-blind-replay and bounded resources when deleting APIs.
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
Run `scripts/smoke.sh` for the headless executable's offline submit/reopen/preflight
gate; it does not qualify live providers, native mutation, or the terminal.
Turn-engine regressions live in `crates/ion-core/tests/c1_*.rs` and in the crate's own
`#[cfg(test)]` modules where a durable pre-state or a storage fault is required.
For documentation-only work, verify links, authority/status consistency and preservation;
do not claim runtime or live-model validation that was not performed.
