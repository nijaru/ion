# Working on Ion

Ion's target is a provider-neutral Rust coding agent: one primary conversation by default, with optional cooperating worker conversations and a first-class TUI. Current Pico/Pi 2 is a leading minimal-harness reference; Codex is a production-engineering reference. Neither is a compatibility target. Existing Ion code is evidence and source material, not an architectural constraint.

## Read and choose work

- `DESIGN.md` owns the accepted core architecture and vocabulary.
- `docs/core-runtime-migration.md` is the active clean rewrite plan.
- `docs/source-layout.md` owns source/module organization.
- `docs/r0-kernel-gates-2026-09-12.md` records the accepted pre-rewrite evidence.
- `ROADMAP.md` owns work order, validation status and later subsystem passes.
- `TERMINAL.md` owns interaction/control/presentation requirements.
- `docs/research/` and `docs/research.md` record exact source findings and rationale.
- Current legacy source/tests establish what the old binary implemented and provide regression evidence; they do not override the target.

Check recent commits/status before editing because the rewrite is active. Proposed, implemented and validated are distinct states.

## Current rewrite rule

R0.1–R0.5 are closed. Do **not** deepen or gradually translate the legacy lane/agent/operation/effect runtime and do not reopen the gates merely because old code has a different shape.

The target core is:

```text
Session
  Conversation
    Entry
    Input
    Task
```

Workers are owned conversations. History parentage, task ownership/dependencies, workspace binding and communication are separate relationships. There is no separate durable Agent object or generic Effect object.

Accepted kernel contracts:

1. async typed `execute/recover/abort` tasks with complete durable checkpoints, invocation-generation fencing, durable cancellation mark + local signal, and a fresh abort invocation;
2. immutable transcript entries with derived heads/edits and stable safe fork cutoffs;
3. task-level external recovery without a generic Effect lifecycle;
4. one private session-local monotonic sequence backing distinct typed local IDs and commit cursors;
5. a small independent provider-neutral `ion-ai` contract crate with scripted model service.

Follow `ROADMAP.md` §1 for current work order: the 2026-09-13 review prioritizes correctness repairs, bounded storage and a measurable single-agent coding loop before further joined-worker/control expansion. K/P numbers are subsystem labels, not permission to bypass that gate. Preserve the core architecture; do not reopen R0 or build another runtime to address local defects. Replace `ion-core` directly rather than maintaining old/new production runtimes. Git history is the archive. Preserve invariants/failure cases from old tests; port leaf algorithms only after their new boundary exists.

Follow `docs/source-layout.md`. Do not recreate broad `runtime.rs`, `manager.rs`, `common.rs`, `utils.rs`, or giant SQL/TUI buckets. A module should have one semantic owner and few reasons to change; file size is a review signal, not something to game by moving code into generic helper files.

## Scope discipline

The core roadmap excludes long-term/project knowledge, memory systems, shared task boards, vector stores and planner layers. Do not shape the core around them. They require separate effectiveness evidence after the baseline agent works.

Do not prematurely redesign every peripheral subsystem during the kernel rewrite. The roadmap schedules first-principles passes for execution/tools, production AI/providers/auth, TUI, extensions/MCP, external protocols and the application shell after the relevant core boundary exists.

## Changes

For each slice, name the observable behavior, semantic owner, failure/recovery boundary and acceptance test. Review findings are source-derived until reproduced; add the boundary-specific regression before marking a repair closed, and record its commit/evidence in `ROADMAP.md`. Keep current status sections consistent with the evidence log; historical green tests do not prove newly identified gaps closed. Prefer the smallest coherent primitive that preserves the target invariant.

A temporary prototype needs an explicit promotion/deletion rule. Do not create permanent duplicate task frameworks, transcript authorities, storage backends or runtime paths. R0 prototype code is deleted once equivalent fresh-core invariants are covered.

When a contract changes, update `DESIGN.md`; when work order/evidence changes, update `ROADMAP.md`; put detailed comparisons/source findings in `docs/research/` rather than turning instructions into a second architecture document.

The `last-go` tag and `docs/history/` are historical recovery/reference material, not acceptance targets.

## Validation

For Rust changes use the checked-in toolchain and run:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

Run smaller targeted tests during iteration, but do not claim a Rust slice validated until the required repository gates pass.

Crash/cancellation/storage/provider work needs deterministic fault tests appropriate to the boundary. Terminal changes additionally require relevant reducer/PTY checks and `scripts/smoke.sh`; human terminal behavior is not established by unit tests alone.

For documentation-only changes, validate authority/status consistency and do not claim compiler/runtime/live-model checks that were not run. Performance/effectiveness claims require measurements.