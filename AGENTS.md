# Ion Agent Instructions

Ion is a Rust terminal coding agent: a small model-facing agent loop
inside a durable, single-writer session runtime. v0.0.0. No Pi or Go
compatibility; reference agents are evidence, never contracts.

The authoritative target design is `DESIGN.md` in this repository. It
owns the product definition, invariants, ownership, domain vocabulary, schema,
and implementation order (§20). Read §§1–3 first when implementing. Pi 2 on
its dev branch, DSH/Cordis, Codex and other relevant agents are references;
the installed Pi distribution is not the sole design authority.

## Session start

Use `mem --json context "Ion current objective and constraints"` first.
Legacy working context (brief, decisions, journal) lives in the
central repository at
`~/github/nijaru/agent-context/projects/github.com/nijaru/ion/ai/`
(load the `ai-context` skill for resolution rules; never recreate a
repository-local `ai/`). Before claims or choosing work:

    sed -n '1,80p' DESIGN.md
    tk ready
    git log --oneline -10
    git status --short

Do the highest-priority unblocked `tk` task.

## Authority

1. Current Rust source and tests.
2. `DESIGN.md` and the ready `tk` task for current work.
3. Central `decisions.md` for rationale still in force.
4. Central `brief.md` for current state.

If `DESIGN.md` and the implementation disagree, resolve against §§1–3
and record the intentional contract and rationale with the verified change.
Legacy briefs are historical evidence; reconcile their claims against source
and the current task before using them.
Changing `DESIGN.md` is itself a decision.

Tag `last-go` is a recovery snapshot, not a design or acceptance
reference; inspect it only when the user explicitly asks.

## Substantial change

1. Record the observable invariant from `DESIGN.md` or a primary source.
2. Name the Ion owner, lifecycle, failure/recovery, and acceptance check.
3. Implement that contract. Delete the obsolete path in the same change.
4. Prove it with tests, then run the matching gates.
5. Update `DESIGN.md` when the contract changes and the `tk` log with
   decisions, verification, and unresolved work. Follow `ai-context` and
   the current memory policy for continuity; do not add new legacy context.
   Commit the coherent chunk.

Follow `DESIGN.md` §20 order. Work order: correctness and ownership,
safety, daily-driver UX through one runtime, providers and integrations,
polish. Do not skip a recorded blocker for a more visible slice. Do not
rewrite working code for style.

## Invariants

Full set with rationale: `DESIGN.md` §§3–16. Non-negotiables:

- One owner per authoritative state; a loaded session has exactly one
  mutation authority.
- Accepted intent is durable before acknowledgment; no repeat-sensitive
  effect starts without a durable effect intent; never silently replay a
  possibly mutating effect.
- Partial model output is never completed assistant content.
- Frontends consume one runtime contract and never write the session
  store.
- Local semantic state is canonical; provider opaque state is
  acceleration, never sole meaning.
- Let errors propagate; never silently downgrade a failed write,
  approval, cancel, teardown, or provider request.
- No duplicate runtimes, transcripts, event streams, or cleanup paths.
- Tokio tasks exist for runtime ownership, not code organization.
- No speculative abstractions, compatibility aliases, or temporary v2
  files.

## Checks

    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
    cargo test --locked --workspace
    scripts/smoke.sh   # before any dogfood request; tmux-based daily-driver flows

Match deeper checks to the slice (DESIGN.md §18): transition tests,
crash injection, PTY, allow/deny, cancel, shutdown, non-interactive.
Performance claims need measurements.