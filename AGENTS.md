# Ion Agent Instructions

## Mission

Ion is a Rust terminal coding agent. The product target is a Pi-like
minimal surface with common agent-runtime primitives built in: a single
agent loop, persistence and resume, streaming, tools, cancellation and
steering, bounded subagents, MCP, ACP, skills/instructions, and
extensions. Implement those contracts idiomatically in Rust. Do not port
or preserve the deleted Go tree.

Ion is v0.0.0. Clean breaks are allowed. There are no Pi or Go
compatibility requirements: no Pi file formats, arguments, JSONL/session
protocols, package names, Go package layout, or compatibility shims.

The required core workflow is:

submit → stream → tool call/result → steer/follow-up → cancel/error → persist → replay/resume

It must be correct under normal use, restart, cancellation, provider
failure, tmux, and live providers.

## Session start

Run these before making claims or choosing work:

    cat ai/brief.md
    tk ready
    git log --oneline -10
    git status --short

Read `ai/research/ion-rust-rewrite-handoff.md` before structural work.
Repo-local Rust source, tests, `ai/`, and tk outrank stale chat context.

Continue the highest-priority unblocked task. Do not reopen completed
design work or inspect `last-go` unless the user explicitly asks to
recover that snapshot.

## Product and reference posture

Pi is the closest behavioral reference because it is a provider-agnostic
agent harness. Use its current source and SDK/release documentation to
recover invariants and solved product problems. Also inspect Codex
CLI/Desktop, Claude Code, Cursor, Zed, and other strong references for
safety, CLI/TUI, review, settings, integrations, and workflow patterns.

References provide evidence about behavior and tradeoffs, not a
specification to copy. Translate the invariant into an Ion-owned contract
and choose the idiomatic Rust implementation. A reference feature is not
automatically an Ion requirement; a missing reference feature is not
automatically a reason to add one.

Subagents and similar runtime primitives are baked into the design, not
bolted on later as extensions. In trusted workspaces, child agents inherit
the user's host capabilities so they can reproduce failures, run tests,
and diagnose systems freely.

The deleted Go implementation, its tests, history, docs, and tasks are
not a product, architecture, regression, schema, or acceptance reference.
The recoverable snapshot is Git tag `last-go`. Do not inspect, audit,
port, or migrate it unless the user explicitly authorizes that recovery.

## Required order of work

For any substantial product or structural change:

1. Reference research: inspect authoritative current source or docs and
   record the observable behavior and invariant.
2. Ideal target: define the desired Ion behavior, ownership, lifecycle,
   failure semantics, safety semantics, and UX before treating existing
   code as a design guide. The rewrite handoff is the starting target
   until Phase 0 replaces it with an approved Rust design.
3. Implementation: fix ownership and contracts first. Delete obsolete
   paths in the same change. Prefer a full rewrite of a bad subsystem to
   wrappers or transitional layers.
4. Behavioral proof: add tests for the target contract, then run the
   affected Rust, TUI/tmux, provider, security, and performance gates.
5. Persist and save: update `ai/brief.md`, `ai/decisions.md`,
   `ai/journal.md`, and tk when state or architecture changes materially;
   commit each coherent working chunk.

Before editing a substantial slice, write down the target invariant, its
single Ion owner, the failure/recovery behavior, and the behavioral
evidence that will close the slice.

## Priority and scope

Choose the next slice from the highest-priority open gate:

1. Correctness and ownership: lifecycle, persistence, replay/resume,
   cancellation, failure recovery, and resource cleanup.
2. Safety: explicit permission, canonical action identity, auditability,
   and fail-closed behavior.
3. Core daily-driver UX: submit/stream/steer/follow-up in TUI, CLI, and
   print modes through the same runtime contract.
4. Provider and integration completeness: real adapters,
   auth/configuration, MCP, ACP, and useful host capabilities.
5. Measured performance and polish.

Do not use lower-priority polish to conceal an unresolved correctness or
safety gate. Deferred work is acceptable only when it is recorded in tk
with an owner, reason, and unblock condition.

## Quality bar

- One owner for every invariant, state transition, and persistence guarantee.
- No duplicate domain models, event streams, transcripts, caches, policy
  engines, runtime composition roots, or cleanup paths.
- No speculative abstractions, dead configuration, compatibility aliases,
  fallback branches, or temporary v2 files.
- Prefer simple, explicit Rust: small crates, concrete types at ownership
  boundaries, narrow traits at consumption boundaries, structured
  cancellation, typed errors, deterministic ordering, bounded queues, and
  clear shutdown semantics.
- Let errors propagate. Recover only to add useful context or to implement
  a defined recovery contract. Never silently downgrade a failed write,
  approval, cancellation, teardown, or provider request.
- Performance claims require benchmarks or profiles.
- Tests must verify behavior and invariants.

## Rust checks

Once a workspace exists, the default gates are:

    cargo fmt --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace

TUI changes also need deterministic reducer tests plus tmux acceptance.
Provider or lifecycle changes need fake, failure-injection, restart, and
live-provider evidence. Security changes need allow/deny, boundary,
cancellation, shutdown, and non-interactive tests.

## Work tracking

Use tk for multi-step work. The active rewrite task is `tk-q99i`. Keep
tasks atomic, demoable, and acceptance-tested. Log findings while fresh.

## Active context files

- `ai/research/ion-rust-rewrite-handoff.md` — starting product and architecture target
- `ai/research/ion-rust-current-source-research-packet.md` — external Rust/protocol research
- `ai/research/agent-reference-matrix.md` — reference evidence, not a spec
- `ai/brief.md` — current state
- `ai/decisions.md` — durable architectural decisions
- `ai/journal.md` — append-only evidence
- `handoff.md` — ephemeral next-session baton
- `.tasks/` via tk — executable work queue
