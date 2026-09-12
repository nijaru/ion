# Working on Ion

Ion's target is a provider-neutral Rust coding agent: single-agent by default, with optional cooperating workers and a first-class TUI. Pi/Pico is the leading reference, not a compatibility requirement. Existing code is reusable evidence, not an architectural constraint.

## Read and choose work

- `DESIGN.md` owns the proposed architecture, domain vocabulary, ownership, and runtime contracts.
- `TERMINAL.md` owns single-agent and group interaction, control, and presentation.
- `ROADMAP.md` owns the next work item, prototype gates, capability coverage, and validation status.
- `docs/research.md` records exact sources, rationale, alternatives, and unresolved decisions.
- Current source/tests establish what the binary implements. `docs/history/` and older central briefs describe the preceding design, not another target.

Start with the roadmap's current position and immediate work section, then read the design sections relevant to the task. Check `git status --short` and recent commits before editing. Consult existing `tk` tasks where available, but reconcile their scope with this roadmap before choosing work; an old Pi-parity priority does not override the new target.

The immediate architecture work is P1 with a thin P4 TUI trace. Resolve a gate with a small executable test and an evidence entry; do not turn unresolved details into broad, unvalidated implementation. Proposed, implemented, and validated are different states. Do not claim the target is already present because a related old feature exists.

## Changes

Name the observable behavior, its owner, its failure/recovery boundary, and its acceptance test. Keep a coherent production path and preserve useful regressions. Reuse or replace components based on the target, not style alone. Prototype code needs a promotion or removal decision.

Update the owning design when a contract changes and the roadmap when evidence changes. Keep detailed source findings in the research record rather than duplicating architecture in instructions. A deliberate departure from an upstream reference is permitted; record the reason.

The `last-go` tag is historical recovery material, not an acceptance reference. Do not restore it or derive new requirements from it without an explicit task.

## Validation

For Rust changes, use the checked-in toolchain and run the relevant tests plus:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

Terminal changes also require relevant PTY/reducer checks and `scripts/smoke.sh` before a dogfood request. Reopen human terminal acceptance when behavior that requires it changes. Match crash, cancellation, permission, storage, and provider tests to the slice. Performance claims need measurements.

For documentation-only changes, validate links, references, authority, and status consistency. Preserve previous authoritative documents in Git/history when replacing them. Do not report compiler, runtime, or live-model checks that were not run.
