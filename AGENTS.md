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

Current stabilization policy:

- Keep the Canto/Ion split. Do not permanently merge Canto into Ion as a shortcut.
- Treat Ion as the acceptance test for Canto until Ion's native minimal agent loop is stable.
- Do not expand Canto as a public/general framework while Ion exposes core-loop bugs.
- Fix framework-owned defects in Canto first, then import the exact Canto revision into Ion.
- Prefer targeted rewrites of broken modules over bug-slice patching or whole-repo rewrites.
- If a module acts as a second agent loop, second transcript writer, hidden session materializer, or unbounded feature host inside the native path, redesign that module from the desired final shape.

## What ion is

A standalone terminal coding agent — same category as Claude Code, Codex, pi. Talks directly to LLM APIs, manages its own tools/memory/sessions. **Not a wrapper. Not a bridge.**

**Primary path** (all new features go here first):
```
ion TUI → CantoBackend → canto framework → provider API (Anthropic, OpenAI, OpenRouter)
```

**Secondary path** (CLI/subscription compatibility, best-effort feature parity):
```
ion TUI → ACPBackend → ACP JSON-RPC 2.0 → [claude | gemini | gh] CLI
```

ACP is for CLI-backed providers and subscription-style access. It does not drive ion's design. When something is unclear, make native mode work first, then bridge the same product behavior to ACP where it fits.

## Active Components

| Component | Purpose |
| --------- | ------- |
| `internal/app` | Bubble Tea v2 host UI: transcript, composer, viewport, and footer. |
| `AgentSession` | Canonical host-facing boundary (SubmitTurn, Events, Cancel). |
| `CantoBackend` | Primary agent core — canto framework, full feature set. |
| `ACPBackend` | CLI/subscription bridge — spawns CLI, bridges via ACP JSON-RPC 2.0. |
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
- Core agent loop stability is the first product priority. Before expanding SOTA features, model routing, subagents, workflows, evals, memory, or provider experiments, make sure the submit -> stream -> tool -> approval -> cancel -> error -> persist/replay loop is reliable and covered by tests.
- Use scriptable CLI/print mode as the first automation surface for core-loop regressions. When TUI behavior is in scope, use `tmux` or another PTY capture and read the captured terminal text to exercise the real Bubble Tea UI: launch header, help, local commands, live turn spacing, `--continue`/`/resume` replay, duplicate transcript symptoms, stale status lines, and footer rendering. Only use image screenshots when a visual-specific layout issue cannot be judged from terminal text.
- Work from the high-level core-loop design down to code. Do not keep fixing isolated bug slices unless the file group has been reviewed against the active audit plan.
- Use priority bands as guidance, not rigid tiers: core loop first; reliability table stakes such as minimal resume/continue and compaction when they protect context survival; product table stakes next; polish later; experimental/SOTA ideas last.
- Advanced ideas from pi, Claude Code, Codex, OpenCode, Cursor, Droid, Letta, and similar agents are references, not mandates. Adopt them only when they simplify Ion or clearly improve the core coding workflow.
- For core-loop and TUI planning/review, compare against local references when useful: `/Users/nick/github/badlogic/pi-mono` as the simple reliable baseline, `/Users/nick/github/openai/codex` as the richer open-source CLI/session/tooling baseline, and Claude Code / Claude-like implementations as product references. Use references to clarify behavior, not to copy features wholesale.
- Prefer simple, inspectable UX over hidden automation. Pi's success with a small clever surface is an explicit design constraint, but Ion may add SOTA capabilities when they preserve that simplicity.
- Use `tk` for all multi-step work.
- When a user reports a bug, create or update a `tk` task immediately.
- If a fix requires touching canto, treat `github.com/nijaru/canto` as the source of truth. Keep framework fixes upstreamable and do not depend on a sibling checkout or bake ion-specific assumptions into canto.
- During core-loop stabilization, develop Canto and Ion as tightly coupled local workstreams: audit/fix Canto-owned contracts in `../canto`, run Canto tests, commit/push coherent Canto fixes when requested/appropriate, then update Ion's Canto dependency and re-run Ion tests.
- Do not do another broad `ai/` sweep unless a specific subsystem points to stale or conflicting context. Use `ai/STATUS.md`, `ai/PLAN.md`, and the active core-loop reset/audit docs, then move to source review.
- Ion is `v0.0.0` unstable. There are no backwards guarantees.
- Do not add fallback, migration, or compatibility paths unless the user explicitly asks for them.
- Do not commit unless the user asks for a commit. Keep coherent changes staged or unstaged for review instead of committing by default.
- Keep global Ion files under `~/.ion/`.
- Stable user-editable settings belong in `~/.ion/config.toml`: defaults, custom endpoints, policy/subagent paths, cost limits, verbosity.
- Mutable runtime choices belong in `~/.ion/state.toml`: selected provider/model/preset/thinking and recent picker state.
- Workspace trust belongs in `~/.ion/trusted_workspaces.json`, separate from config and state.
- Do not persist provider/model automatically at startup; only user edits and explicit TUI actions should write settings/state.
- Prefer hardcoded defaults for ion-owned behavior. Add persistent state only when the value is machine-owned and genuinely needs to survive sessions.

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
- `status-and-config.md` — status line specs, config source of truth, model metadata rules

**Historical Rust docs:**

- `archive/rust/docs/`
