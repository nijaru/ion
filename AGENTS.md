# Ion Agent Instructions

Ion is a Rust terminal coding agent: a deliberately small model-facing
agent loop inside a rigorous, durable, single-writer session runtime.
Runtime primitives are built in: one agent loop, persist/resume,
streaming, tools, cancel/steer, bounded child sessions, MCP, ACP,
skills/instructions, and extensions.

v0.0.0. No Pi or Go compatibility: no Pi file formats, arguments,
JSONL/session protocols, package names, or shims. Reference agents are
evidence, never contracts.

Core journey (DESIGN.md §1.1):

open/create session → accept user intent durably → project model context
→ model generation → tool/effect cycles → steer/follow-up/approval/
cancellation → settle operation → close → reopen → recover/continue

## Session start

The authoritative target design is `DESIGN.md` in this repository.
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

Do the highest-priority unblocked `tk` task. Current source, tests,
`DESIGN.md`, the central `brief.md`, and `tk` outrank chat and any
imported research. There is no central `research/` tree anymore;
external evidence lives in `DESIGN.md` §35 and its cited sources.

## Authority

1. Current Rust source and tests.
2. `DESIGN.md` (this repo) and the ready `tk` task for current work.
3. Central `decisions.md` for rationale still in force.
4. Central `brief.md` for current state.

When `DESIGN.md` and the implementation disagree: if the code
intentionally improved on the design, update `DESIGN.md` in the same
change; if the code drifted accidentally, restore the invariant.
Changing `DESIGN.md` is itself a decision — record it in central
`decisions.md`.

Tag `last-go` is a recovery snapshot, not a product, architecture,
regression, schema, or acceptance reference. Inspect it only when the
user explicitly asks.

Pi, Codex, Claude Code, Cursor, Zed, OTP, and durable-execution systems
are behavioral references. Translate an invariant into an Ion-owned
contract. A reference feature is not an Ion requirement.

## Substantial change

1. Record the observable invariant from `DESIGN.md` or a current primary
   source.
2. Name the Ion owner, lifecycle, failure/recovery, and acceptance check.
3. Implement that contract. Delete the obsolete path in the same change.
4. Prove it with tests, then run the matching gates.
5. Update `DESIGN.md` if the design changed, central `brief.md`,
   central `decisions.md` if rationale changed, central `journal.md`
   for the factual outcome, and the `tk` log. Commit the coherent chunk.

Follow `DESIGN.md` §32 implementation order. Choose work in this order:
correctness and ownership, safety, daily-driver UX through one runtime,
then providers and integrations, then polish. Do not skip a recorded
blocker for a more visible slice. Do not rewrite working code for style
(DESIGN.md §29.3).

## Invariants

- One owner for each authoritative state, transition, and durable write;
  a loaded session has exactly one mutation authority (`SessionRuntime`).
- Accepted intent is durable before its acknowledgment (P4).
- No repeat-sensitive external effect starts before its durable effect
  intent; never silently replay a possibly mutating effect (P5).
- Only validated completed provider output becomes a completed assistant
  entry; partial output is never completed content.
- Frontends consume the same runtime contract. The TUI does not own
  agent truth and never writes the session store.
- Local semantic state is canonical; model context is a derived
  projection; provider opaque state is acceleration, never sole meaning.
- Trusted children inherit host capabilities. Do not invent a sandbox
  and then pretend it exists.
- Let errors propagate. Never silently downgrade a failed write,
  approval, cancel, teardown, or provider request.
- No duplicate runtimes, transcripts, event streams, or cleanup paths.
- Tokio tasks exist for runtime ownership/lifecycle, not code
  organization; no `Arc<Mutex<SessionState>>`.
- No speculative abstractions, compatibility aliases, or temporary v2
  files.

## Checks

    cargo fmt --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace

Also: operation transitions → exhaustive pure transition tests; crash
windows → injection matrix (DESIGN.md §30.2); lifecycle/provider → fake,
failure-injection, restart; TUI → reducer tests plus tmux/PTY; safety →
allow/deny, cancel, shutdown, non-interactive. Performance claims need
measurements.
