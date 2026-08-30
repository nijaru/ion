# Ion Agent Instructions

Ion is a Rust terminal coding agent: a small model-facing agent loop
inside a durable, single-writer session runtime. v0.0.0. No Pi or Go
compatibility; reference agents are evidence, never contracts.

The authoritative target design is `DESIGN.md` in this repository. It
owns the core journey, principles (P1–P14), ownership table, domain
vocabulary, schema, and implementation order (§32). Read its §0.1 first
when implementing.

## Session start

Persistent working context (brief, decisions, journal) lives in the
central repository at
`~/github/nijaru/agent-context/projects/github.com/nijaru/ion/ai/`
(load the `ai-context` skill for resolution rules; never recreate a
repository-local `ai/`). Before claims or choosing work:

    sed -n '1,80p' DESIGN.md
    cat ~/github/nijaru/agent-context/projects/github.com/nijaru/ion/ai/brief.md
    tk ready
    git log --oneline -10
    git status --short

Do the highest-priority unblocked `tk` task.

## Authority

1. Current Rust source and tests.
2. `DESIGN.md` and the ready `tk` task for current work.
3. Central `decisions.md` for rationale still in force.
4. Central `brief.md` for current state.

If `DESIGN.md` and the implementation disagree, resolve per DESIGN.md §0
and record an intentional design change in central `decisions.md`.
Changing `DESIGN.md` is itself a decision.

Tag `last-go` is a recovery snapshot, not a design or acceptance
reference; inspect it only when the user explicitly asks.

## Substantial change

1. Record the observable invariant from `DESIGN.md` or a primary source.
2. Name the Ion owner, lifecycle, failure/recovery, and acceptance check.
3. Implement that contract. Delete the obsolete path in the same change.
4. Prove it with tests, then run the matching gates.
5. Update `DESIGN.md` if the design changed, central `brief.md`,
   `decisions.md` if rationale changed, `journal.md` for the factual
   outcome, and the `tk` log. Commit the coherent chunk.

Follow `DESIGN.md` §32 order. Work order: correctness and ownership,
safety, daily-driver UX through one runtime, providers and integrations,
polish. Do not skip a recorded blocker for a more visible slice. Do not
rewrite working code for style (§29.3).

## Invariants

Full set with rationale: `DESIGN.md` P1–P14 and §31. Non-negotiables:

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

Match deeper checks to the slice (DESIGN.md §30): transition tests,
crash injection, PTY, allow/deny, cancel, shutdown, non-interactive.
Performance claims need measurements.