# Ion Agent Instructions

Ion is a Rust terminal coding agent. Target a Pi-like surface with
runtime primitives built in: one agent loop, persist/resume, streaming,
tools, cancel/steer, bounded subagents, MCP, ACP, skills, and extensions.
Implement those contracts in idiomatic Rust. Do not port tag `last-go`.

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

Then do the highest-priority unblocked `tk` task. Do not start a Cargo
workspace until Phase 1 (`tk-nwrx`) is ready.

Current source, tests, `ai/brief.md`, and `tk` outrank chat and imported
handoffs. Read a research file only when the ready task needs it.

## Source rank

1. Current Rust source and tests, once they exist.
2. `ai/brief.md` and the ready `tk` task.
3. `ai/decisions.md` for durable rationale still in force.
4. `ai/research/ion-rust-rewrite-handoff.md` — product/architecture
   starting target until Phase 0 replaces it.
5. `ai/research/ion-rust-tui-handoff.md` — TUI-layer research only.
6. `ai/research/ion-rust-current-source-research-packet.md` — versions
   and protocol notes; recheck before use.
7. `ai/research/agent-reference-matrix.md` — evidence, not a spec.

Do not inspect, audit, port, or migrate tag `last-go` unless the user
explicitly asks to recover that snapshot.

Pi, Codex, Claude Code, Cursor, and Zed are behavioral references.
Translate an invariant into an Ion-owned contract. A reference feature is
not an Ion requirement.

## Roadmap

Parent: `tk-q99i`. Ready work comes from `tk ready`, not this list.

| Phase | Task | Outcome |
| --- | --- | --- |
| 0 | `tk-mggy` | Target matrix; no code |
| 1 | `tk-nwrx` | One print-mode turn through `RuntimeController` |
| 2 | `tk-m18r` | Scripted tool loop |
| 3 | `tk-tog9` | SQLite sessions and replay |
| 4 | `tk-1vz6` | Ratatui TUI on that runtime |
| 5 | `tk-m6qk` | Bounded multi-agent, after sessions |
| 6 | `tk-e9dg` | MCP and ACP |
| 7 | `tk-7cd6` | Subprocess extensions |
| 8 | `tk-bemr` | Optional WASM; not a ship gate |

The sequence can change. Do not skip a blocker to start a more visible
slice. TUI work uses the TUI handoff: Ratatui substrate, Ion-owned
reducer, inline default. Do not start from `rnk`, `iocraft`, `tui-realm`,
or a custom Crossterm compositor.

## Substantial change

1. Record the observable invariant from a current primary source.
2. Name the Ion owner, lifecycle, failure/recovery, and acceptance check.
3. Implement that contract. Delete the obsolete path in the same change.
4. Prove it with tests, then run the matching gates.
5. Update `ai/brief.md`, `ai/decisions.md` if rationale changed,
   `ai/journal.md` for the factual outcome, and the `tk` log. Commit the
   coherent chunk.

Choose work in this order: correctness/ownership, safety, daily-driver
UX through one runtime, then providers/integrations, then polish.

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

## Context files

- `ai/brief.md` — current head, ready task, next action
- `ai/decisions.md` — rationale still in force
- `ai/journal.md` — cold history; do not load at startup
- `ai/research/index.md` — research map
- `handoff.md` — ephemeral baton if present
- `.tasks/` via `tk` — executable queue
