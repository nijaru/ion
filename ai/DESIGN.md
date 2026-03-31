# ion Design

Updated: 2026-03-28

## Product boundary

ion is a standalone terminal coding agent.

- Native runtime is the primary product:
  - `ion TUI -> CantoBackend -> canto -> provider API`
- ACP is a secondary bridge for subscription access:
  - `ion TUI -> ACPBackend -> ACP CLI process`

Native runtime drives design. ACP follows where possible.

## Main boundaries

| Layer | Responsibility | Component |
| --- | --- | --- |
| **4** | **Application** | `ion` (TUI, Workspace logic, UX Policy, Model Pickers) |
| **3** | **Framework** | `canto` (Agent loop, `safety` engine, `x/tools` library, Context Governor, Session Log) |
| **2** | **Logic** | `llm` (Provider abstraction, Token counting, Cost calculation) |
| **1** | **Transport** | `http` (API clients, JSON-RPC, SSE) |

## SOTA & Minimalist Goals

`ion` aims for SOTA (State of the Art) capabilities with a minimalist, terminal-first UX:
- **Safety by Default:** Powered by `canto/safety`, with 3-mode operation (READ/EDIT/YOLO).
- **Infinite Context:** Managed by a background "Context Governor" in `canto` (Layer 3).
- **Subagent Spawning:** First-class support for child agents via `canto` primitives.
- **MCP Extensibility:** Integration with MCP servers for rich tool ecosystems.

## TUI architecture

### Modular Component Design

The TUI is being refactored into isolated sub-models to improve maintainability and testing:
- **`Viewport`:** Purely for rendering the committed terminal scrollback.
- **`Input`:** Manages the textarea, history, and status bar.
- **`Broker`:** A headless component managing the backend connection and event translation.

## Runtime/session boundary

The TUI consumes backend interfaces. That separation is real inside the repo.

Current limitation:

- ion is not yet a clean reusable external runtime library
- bootstrap/orchestration still lives mainly in `cmd/ion/main.go`
- host/runtime contracts still depend on ion-owned `internal/` packages

Implication:

- a headless `ion --agent` path is more realistic than trying to export the current runtime surface directly

## Provider architecture

Provider behavior is catalog-driven.

The provider catalog owns:

- provider ids
- display names
- kind/grouping
- runtime family
- auth kind
- default env vars
- default endpoints
- picker visibility and setup hints

Current native coverage is broad. The important remaining work is validation, not another structural refactor.

## Prompt and instruction layering

Keep these distinct:

1. ion core system prompt
2. runtime/session context
3. repo-local instruction files
4. optional future skills
5. task/mode reminders

Current repo-local instruction loading supports:

- `AGENTS.md`
- `CLAUDE.md`

It does not yet support first-class skills.

## Current technical debt worth tracking

- `tk-ekao` — provider-by-provider auth and fetch validation
- `tk-lmhg` — define real skill support instead of conflating it with instruction files
- `tk-5t72` — dual storage in `CantoBackend`
- `tk-9n7h` — reevaluate `internal/backend/registry`
- `tk-st4q` — clean headless ACP-agent mode
