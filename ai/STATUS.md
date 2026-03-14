# ion Status

## Current State

| Metric                  | Value                                                                                | Updated    |
| ----------------------- | ------------------------------------------------------------------------------------ | ---------- |
| Phase                   | Go rewrite active on main                                                            | 2026-03-13 |
| Status                  | `go-host/` flattened to repo root; module is now `github.com/nijaru/ion`             | 2026-03-13 |
| Active implementation   | `internal/` (repo root)                                                              | 2026-03-13 |
| Host state              | Transcript, multiline composer, streamed backend scaffold, tool rendering, app tests | 2026-03-13 |
| Historical checkpoint   | `stable-rnk` tag preserved at the last stable RNK-era mainline                       | 2026-03-13 |
| Archived implementation | Rust code and Rust-TUI docs moved under `archive/rust/`                              | 2026-03-13 |

## Active Work

1. `tk-3bd5` (p1): umbrella Go rewrite program on `main`
2. `tk-n1al` (p1): harden the scripted backend against the session interface
3. `tk-vmdl` (p2): add transcript/session persistence to the Go host (In Progress: `internal/storage` implemented)
4. `tk-mlhe` (p2): build the ACP backend adapter layer
5. `tk-qjs2` (p2): design memory and context architecture for the rewrite
6. `tk-npsw` (p2): design subagents, swarms, and RLM runtime patterns

## Current Findings

- Bubble Tea v2 is now the chosen host direction, not an evaluation branch.
- The host targets an ACP-shaped `AgentSession` interface so both native ion execution and external agents fit behind one boundary.
- **Session Persistence:** `internal/storage` implemented using JSONL for append-only events and SQLite for indexing (`index.db`) and input history (`input.db`). Supports encoded CWD paths for isolation.
- **Multiplexed Swarms:** `session.Event` interface updated with `BaseEvent` (`AgentID`, `TraceID`) to support parallel agent streams and task trees.
- **Objective Functions:** `EventVerificationResult` added to support RLM/autoresearch loops (test/benchmark results).
- **Model Discovery:** Adopted **Catwalk + API Hybrid** approach. Use `charm.land/catwalk` for static metadata (context, pricing) and Provider APIs for live availability. Replaced `models.dev` dependency.
- **Repo structure:** `go-host/` flattened to repo root. `cmd/ion/`, `internal/`, `go.mod` now at top level. Run: `go run ./cmd/ion`.
- **SOTA 2026 Research:** Synthesized core patterns from Claude Code, Slate (Realmcore), autoresearch, and Letta:
  - **The 1,000 Token Rule:** Minimal system prompts are the industry standard for context efficiency.
  - **Parallel Tooling:** Programmatic tool execution reduces context bloat and turn latency.
  - **Guidance vs. Execution Agents:** Asynchronous sub-agents are superior for maintaining long-term memory/planning.
  - **Tree-Based Sessions:** Branching and forking history is critical for developer workflows.
- Archived Rust material remains available for historical context and reference, but should not drive new planning.

## Next Steps

1. Finalize `tk-vmdl` by integrating `internal/storage` into `cmd/ion` and `internal/app`.
2. Harden the scripted backend (`tk-n1al`) to support swarm/multiplexed event testing.
3. Build the ACP backend adapter layer (`tk-mlhe`) to support Gemini CLI and Claude Code.
4. Design and implement memory/context architecture for the Go rewrite.

## Key References

| Topic                                | Location                                                  |
| ------------------------------------ | --------------------------------------------------------- |
| Main rewrite task                    | `.tasks/tk-3bd5.json`                                     |
| AgentSession task                    | `.tasks/tk-8j82.json`                                     |
| Host UX task                         | `.tasks/tk-5fcp.json`                                     |
| Host architecture                    | `ai/design/go-host-architecture.md`                       |
| Session interface                    | `ai/design/session-interface.md`                          |
| ACP integration                      | `ai/design/acp-integration.md`                            |
| Native backend                       | `ai/design/native-ion-agent.md`                           |
| Memory and context                   | `ai/design/memory-and-context.md`                         |
| Subagents / swarms / RLM             | `ai/design/subagents-swarms-rlm.md`                       |
| Rewrite roadmap                      | `ai/design/rewrite-roadmap.md`                            |
| Bubble Tea decision note             | `ai/research/bubbletea-v2-vs-rust-tui-host-2026-03-12.md` |
| Rust TUI audit retained as reference | `ai/review/tui-lib-audit-2026-03-11.md`                   |
| Archived Rust implementation         | `archive/rust/`                                           |
