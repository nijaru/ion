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

## What ion is

A standalone terminal coding agent — same category as Claude Code, OpenCode, pi. Talks directly to LLM APIs, manages its own tools/memory/sessions. **Not a wrapper. Not a bridge.**

**Primary path** (all new features go here first):
```
ion TUI → CantoBackend → canto framework → provider API (Anthropic, OpenAI, OpenRouter)
```

**Secondary path** (subscription access only, best-effort feature parity):
```
ion TUI → ACPBackend → ACP JSON-RPC 2.0 → [claude | gemini | gh] CLI
```

ACP is for users whose subscription ToS prohibits direct API use. It does not drive ion's design. When something is unclear, default to making it work in native mode first.

## Active Components

| Component | Purpose |
| --------- | ------- |
| `internal/app` | Bubble Tea v2 host UI: transcript, composer, viewport, and footer. |
| `AgentSession` | Canonical host-facing boundary (SubmitTurn, Events, Cancel). |
| `CantoBackend` | Primary agent core — canto framework, full feature set. |
| `ACPBackend` | Subscription bridge — spawns CLI, bridges via ACP JSON-RPC 2.0. |
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
- Ion is `v0.0.0` unstable. There are no backwards guarantees.
- Do not add fallback, migration, or compatibility paths unless the user explicitly asks for them.
- Keep runtime state in `~/.ion/state.toml`; reserve `~/.ion/config.toml` for hand-edited user settings.

## Go Idioms

Use the `go-expert` skill for full guidance. Key modern idioms:

- `slices` / `maps` packages — not manual loops or `sort.Slice`
- `iter.Seq` / `iter.Seq2` — range-over-function iterators (Go 1.23+)
- `sync.WaitGroup.Go` — replaces `Add(1); go func() { defer Done() }()`
- `errors.AsType[T](err)` — type-safe error unwrapping (Go 1.26)
- `t.Context()` in tests — not `context.TODO()`

## Reference

**Start here:**

- `ai/STATUS.md` — current state, open questions, key file index
- `ai/DESIGN.md` — architecture overview and event flow
- `ai/DECISIONS.md` — append-only architectural decision log

**Topic specs (`ai/specs/`):**

- `subscription-providers.md` — provider table, ToS rationale, backend selection logic
- `acp-integration.md` — ACP protocol, event mapping, known gaps
- `config-and-metadata.md` — status line specs, config source of truth, model metadata rules

**Historical Rust docs:**

- `archive/rust/docs/`
