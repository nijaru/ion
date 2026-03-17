# ion

Go rewrite of a fast, lightweight inline coding agent.

## Architecture & Philosophy

ion is a specialized coding application built on top of the **canto** framework.

- **canto (The Framework):** Provides general-purpose agent primitives (Layer 3): LLM streaming, append-only session logging, agent loop, tool registry, and memory. It is the "Rails" of the agent stack.
- **ion (The Application):** A TUI-based coding environment (Layer 4) that uses canto's primitives to implement a specific developer workflow. It is a "Rails app."

| Layer | Responsibility | Component |
| ----- | -------------- | --------- |
| **4** | **Application** | ion (TUI, Coding tools, Workspace logic) |
| **3** | **Framework** | canto (Session log, Agent loop, Tooling, Memory) |
| **2** | **Logic** | llm (Provider interface, Token counting, Cost) |
| **1** | **Transport** | http (API clients, SSE, JSON-RPC) |

## Active Architecture

| Component | Purpose |
| --------- | ------- |
| `internal/app` | Bubble Tea v2 host UI: transcript, composer, viewport, and footer. |
| `AgentSession` | Canonical host-facing boundary (SubmitTurn, Events, Cancel). |
| `CantoBackend` | The primary agent core powered by the `canto` framework. |
| `ACPAgentSession` | Future support for external agents (Claude Code, etc.) via protocol. |
| `archive/rust/` | Historical reference only; not active implementation guidance. |

## Project Structure

| Directory | Purpose |
| --------- | ------- |
| `cmd/ion/` | Main entry point and CLI flag parsing. |
| `internal/` | Application-specific packages (UI, Backend adapters, Local storage). |
| `ai/` | Active design memory and task context (local-only). |
| `.tasks/` | Task tracking (`tk`) state (local-only). |

## Historical Checkpoint

- Use the Git tag `stable-rnk` for the last known stable Rust/RNK-era mainline.
- Do not move or rewrite that tag.

## Commands

```bash
go test ./...
go run ./cmd/ion

tk ls
 tk ready
 tk show <id>
 tk log <id> "finding"
 tk done <id>
```

## Rules

- Treat Go as the active implementation language.
- Treat `archive/rust/` as read-only reference unless explicitly migrating something out of it.
- Do not let archived Rust docs drive new design decisions on `main`.
- Use `tk` for all multi-step work.
- When a user reports a bug, create or update a `tk` task immediately.

## Go Idioms

Use the `go-expert` skill for full guidance. Key modern idioms:

- `slices` / `maps` packages — not manual loops or `sort.Slice`
- `iter.Seq` / `iter.Seq2` — range-over-function iterators (Go 1.23+)
- `sync.WaitGroup.Go` — replaces `Add(1); go func() { defer Done() }()`
- `errors.AsType[T](err)` — type-safe error unwrapping (Go 1.26)
- `t.Context()` in tests — not `context.TODO()`

## Reference

**Active design docs:**

- `ai/design/go-host-architecture.md`
- `ai/design/session-interface.md`
- `ai/design/acp-integration.md`
- `ai/design/native-ion-agent.md`
- `ai/design/memory-and-context.md`
- `ai/design/subagents-swarms-rlm.md`
- `ai/design/rewrite-roadmap.md`

**Retained research themes:**

- memory and context
- session persistence
- tools and MCP/ACP integration
- subagents, swarms, and RLM patterns

**Historical Rust docs:**

- `archive/rust/docs/`
