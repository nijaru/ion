# Working on Ion

Ion's target is a provider-neutral Rust coding agent: one primary conversation by default, with optional cooperating worker conversations and a first-class TUI. Current Pico/Pi 2 is the leading minimal-harness reference; Codex is a production-engineering reference. Neither is a compatibility target. Existing Ion code is evidence and source material, not an architectural constraint.

## Read and choose work

- `DESIGN.md` owns the current core architecture and vocabulary.
- `docs/core-runtime-migration.md` is the active **clean rewrite plan** despite its historical filename.
- `ROADMAP.md` owns work order, gates, validation status and later subsystem passes.
- `TERMINAL.md` owns interaction/control/presentation requirements.
- `docs/research/` and `docs/research.md` record exact source findings and rationale.
- Current source/tests establish what the old binary implements and provide regression evidence; they do not override the target.

Check recent commits/status before editing because the design is moving quickly. Read the current rewrite gate before touching production code. Proposed, implemented and validated are distinct states.

## Current rewrite rule

Do **not** deepen or gradually translate the legacy lane/agent/operation/effect runtime.

The target core is:

```text
Session
  Conversation
    Entry
    Input
    Task
```

Workers are owned conversations. History parentage, task ownership/dependencies, workspace binding and communication are separate relationships. There is no separate durable Agent object or generic Effect object in the leading design unless a pre-rewrite prototype proves one necessary.

Before deleting/rebuilding the old core, close the five R0 gates in `docs/core-runtime-migration.md`:

1. async `execute/recover/abort` task contract with durable invocation-fenced commits and optional phase helper;
2. immutable entry projection/head/edit context and fork semantics;
3. task-level external recovery without generic Effect, unless disproved;
4. session-local ID/sequence representation;
5. minimal provider-neutral scripted model-service contract.

After those settle, replace `ion-core` directly rather than maintaining old/new production runtimes. Git history is the archive. Preserve invariants/failure cases from old tests; port implementation algorithms only after their new boundary is accepted.

## Scope discipline

The core roadmap excludes long-term/project knowledge, memory systems, shared task boards, vector stores and planner layers. Do not shape the core around them. They require separate effectiveness evidence after the baseline agent works.

Likewise, do not prematurely redesign every peripheral subsystem during the kernel rewrite. The roadmap schedules first-principles passes for execution/tools, AI/providers/auth, TUI, extensions/MCP, external protocols and the application shell after the relevant core boundary exists.

## Changes

For each slice, name the observable behavior, semantic owner, failure/recovery boundary and acceptance test. Prefer the smallest coherent primitive that preserves the target invariant.

A temporary prototype needs an explicit promotion/deletion rule. Do not create permanent duplicate task frameworks, transcript authorities, storage backends or runtime paths.

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