# Ion Agent Instructions

Ion is a Rust terminal coding agent. Target a Pi-like surface with
runtime primitives built in: one agent loop, persist/resume, streaming,
tools, cancel/steer, bounded subagents, MCP, ACP, skills, and extensions.
Implement those contracts in idiomatic Rust.

v0.0.0. No Pi or Go compatibility: no Pi file formats, arguments,
JSONL/session protocols, package names, or shims.

Core workflow:

submit → stream → tool call/result → steer/follow-up → cancel/error → persist → replay/resume

## Session start

Before claims or choosing work:

    cat ai/brief.md
    tk ready
    git log --oneline -10
    git status --short

Do the highest-priority unblocked `tk` task. Current source, tests,
`ai/brief.md`, and `tk` outrank chat and imported research. Load a
research file only when that task needs it. Do not load `ai/journal.md`
at startup.

## Authority

1. Current Rust source and tests, once they exist.
2. `ai/brief.md` and the ready `tk` task for current work.
3. `ai/decisions.md` for rationale still in force.
4. `ai/research/` for product, TUI, protocol, and reference notes.
   Recheck versions and protocol docs before using them.

Tag `last-go` is a recovery snapshot, not a product, architecture,
regression, schema, or acceptance reference. Inspect it only when the
user explicitly asks.

Pi, Codex, Claude Code, Cursor, and Zed are behavioral references.
Translate an invariant into an Ion-owned contract. A reference feature
is not an Ion requirement.

## Substantial change

1. Record the observable invariant from a current primary source.
2. Name the Ion owner, lifecycle, failure/recovery, and acceptance check.
3. Implement that contract. Delete the obsolete path in the same change.
4. Prove it with tests, then run the matching gates.
5. Update `ai/brief.md`, `ai/decisions.md` if rationale changed,
   `ai/journal.md` for the factual outcome, and the `tk` log. Commit the
   coherent chunk.

Choose work in this order: correctness and ownership, safety,
daily-driver UX through one runtime, then providers and integrations,
then polish. Do not skip a recorded blocker for a more visible slice.

## Invariants

- One owner for each authoritative state, transition, and durable write.
- Frontends consume the same runtime contract. The TUI does not own
  agent truth.
- Trusted children inherit host capabilities. Do not invent a sandbox
  and then pretend it exists.
- Let errors propagate. Never silently downgrade a failed write,
  approval, cancel, teardown, or provider request.
- No duplicate runtimes, transcripts, event streams, or cleanup paths.
- No speculative abstractions, compatibility aliases, or temporary v2
  files.

## Checks

When a workspace exists:

    cargo fmt --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace

Also: TUI → reducer tests plus tmux/PTY; lifecycle/provider → fake,
failure-injection, restart; safety → allow/deny, cancel, shutdown,
non-interactive. Performance claims need measurements.
