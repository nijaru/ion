# ion Decisions

## 2026-03-13: Bubble Tea v2 Is the Mainline Host Direction

**Context**: The Rust TUI rewrite consumed significant effort in inline rendering, multiline composer handling, footer contracts, and resize behavior. The Go rewrite branch proved Bubble Tea v2 could stand up the host loop faster and with less fragility.

**Decision**: Adopt the Go/Bubble Tea v2 host as the active mainline direction.

**Rationale**: The project needs a host that is pleasant to evolve and trustworthy in daily use. Bubble Tea v2 now fits that goal better than continuing the custom Rust TUI as the primary path.

---

## 2026-03-13: Use an ACP-Shaped Session Boundary

**Context**: `ion` needs to support both native ion execution and hosted external agents such as Claude Code, Gemini CLI, and later Codex-style backends.

**Decision**: Center the Go rewrite around an ACP-shaped `AgentSession` interface with typed events, while keeping the internal domain model independent from raw ACP wire structs.

**Rationale**: This gives the host one durable abstraction for native and external agents without forcing native execution to literally use ACP wire types internally.

---

## 2026-03-13: Preserve `stable-rnk` as the Historical Rust/RNK Checkpoint

**Context**: The project still needs a trustworthy pointer to the last stable pre-rewrite terminal experience.

**Decision**: Keep the existing `stable-rnk` tag exactly where it is.

**Rationale**: The tag is the canonical recovery point for the RNK-era mainline and should remain stable even as `main` moves to the Go rewrite.

---

## 2026-03-13: Flatten `go-host/` to Repo Root; Defer Multi-Module Split

**Context**: `go-host/` was named to distinguish the Go implementation from the active Rust codebase. With Rust archived, the prefix is vestigial. `src/` is not idiomatic Go. The standard single-binary Go layout puts `cmd/`, `internal/`, `go.mod` at the repo root.

**Decision**: Flatten `go-host/` to the repo root. Rename `cmd/ion-go` → `cmd/ion`. Hold the multi-module split until there is a concrete reason to do it.

**Rationale**: The TUI and native agent are tightly coupled today. A multi-module split (e.g. `host/` + `agent/` each with their own `go.mod`) pays off when the agent runtime needs to run as a standalone binary or daemon — an ACP server, a background session manager, or a separately importable library. Until one of those is true, the split adds build complexity for no gain.

---

## 2026-03-13: Synthesize SOTA 2026 Agent Patterns

**Context**: Research into Claude Code, pi-mono, autoresearch, Crush, Droid, and Letta revealed a consensus on high-performance agent architecture.

**Decision**: Adopt the "1,000 Token Rule" for system prompts, prioritize tree-based session storage for branching history, and implement a Gather-Act-Verify reasoning loop.

**Rationale**: These patterns are proven to increase context efficiency, reasoning stability, and developer productivity across the most successful agentic tools.
