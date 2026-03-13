# ion Status

## Current State

| Metric | Value | Updated |
| --- | --- | --- |
| Phase | Mainline transition to Go rewrite | 2026-03-13 |
| Status | `main` now tracks the Go/Bubble Tea rewrite direction | 2026-03-13 |
| Active implementation | `go-host/` | 2026-03-13 |
| Host state | Transcript, multiline composer, streamed backend scaffold, tool rendering, app tests | 2026-03-13 |
| Historical checkpoint | `stable-rnk` tag preserved at the last stable RNK-era mainline | 2026-03-13 |
| Archived implementation | Rust code and Rust-TUI docs moved under `archive/rust/` | 2026-03-13 |

## Active Work

1. `tk-3bd5` (p1): umbrella Go rewrite program on `main`
2. `tk-8j82` (p1): define the ACP-shaped `AgentSession` interface
3. `tk-5fcp` (p1): make the Go host feel like ion in daily use
4. `tk-n1al` (p1): harden the scripted backend against the session interface
5. `tk-x1yq` (p1): build the first native ion backend in Go
6. `tk-mlhe` (p2): build the ACP backend adapter layer
7. `tk-vmdl` (p2): add transcript/session persistence to the Go host
8. `tk-qjs2` (p2): design memory and context architecture for the rewrite
9. `tk-npsw` (p2): design subagents, swarms, and RLM runtime patterns

## Current Findings

- Bubble Tea v2 is now the chosen host direction, not an evaluation branch.
- The host should target an ACP-shaped session interface so both native ion execution and external agents fit behind one boundary.
- The current Go host is already more stable in multiline editing than the Rust TUI path it replaces.
- The next important transition is replacing the fake backend contract with a real session lifecycle and event model.
- Archived Rust material remains available for historical context and implementation reference, but should not drive new planning.

## Next Steps

1. Replace the current backend contract with the canonical `AgentSession` interface.
2. Make the host shell feel like ion: transcript, composer, footer, progress, resize, and scrolling.
3. Implement the first native ion backend behind that interface.
4. Add ACP-backed external agent support after the host/session boundary is stable.
5. Add persistence, memory/context, and advanced agent-runtime features on top of that foundation.

## Key References

| Topic | Location |
| --- | --- |
| Main rewrite task | `.tasks/tk-3bd5.json` |
| AgentSession task | `.tasks/tk-8j82.json` |
| Host UX task | `.tasks/tk-5fcp.json` |
| Host architecture | `ai/design/go-host-architecture.md` |
| Session interface | `ai/design/session-interface.md` |
| ACP integration | `ai/design/acp-integration.md` |
| Native backend | `ai/design/native-ion-agent.md` |
| Memory and context | `ai/design/memory-and-context.md` |
| Subagents / swarms / RLM | `ai/design/subagents-swarms-rlm.md` |
| Rewrite roadmap | `ai/design/rewrite-roadmap.md` |
| Bubble Tea decision note | `ai/research/bubbletea-v2-vs-rust-tui-host-2026-03-12.md` |
| Rust TUI audit retained as reference | `ai/review/tui-lib-audit-2026-03-11.md` |
| Archived Rust implementation | `archive/rust/` |
