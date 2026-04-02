# ion Design

Updated: 2026-04-01

## Product boundary

ion is a standalone terminal coding agent.

- Native runtime is the primary product:
  - `ion TUI -> CantoBackend -> canto -> provider API`
- ACP is a secondary bridge for subscription access:
  - `ion TUI -> ACPBackend -> ACP CLI process`
- OpenAI ChatGPT subscriptions are a separate platform from the API:
  - if we ever support them, it should be via a distinct ChatGPT-app style bridge, not by assuming API-style access

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
- **Infinite Context:** Managed by a background "Context Governor" in `canto` (Layer 3). Runtime turns auto-recover from context overflow through `governor.RecoveryProvider`, while manual `/compact` uses the non-recovery compaction provider to avoid recursive retries.
- **Subagent Spawning:** First-class support for child agents via `canto` primitives.
- **MCP Extensibility:** Integration with MCP servers for rich tool ecosystems.

Important constraint:
- Pi is a useful maturity target, not a hard feature-parity gate.
- Advanced orchestration work is downstream of a stable, feature-complete single-agent inline loop.
- The solo agent is the core product; subagents, ACP, and swarm views are wrappers around that core.

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

Session metadata now carries a cheap `Title` plus a one-line `Summary`; the resume picker uses `Title -> Summary -> LastPreview` and treats `LastPreview` as fallback, not the primary label.

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

Auth model guidance:

- most providers should stay simple API-key or custom-endpoint entries
- subscription/OAuth providers should be explicit and provider-specific
- CLI-bridge providers stay separate from native API providers
- ChatGPT subscription support, if we ever add it, should be treated as a separate evaluation track rather than assumed to be part of the native API path
- Claude and Gemini subscription workflows should stay behind official CLI or ACP-style bridges when that is the supported path

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

## Pi-mono cross-pollination

Pi-mono analysis complete (see `research/pi-architecture.md`, `design/cross-pollination.md`). Key ion-relevant decisions:

### TUI improvements to adopt
- ~~**Bounding-box diff rendering**~~ — Rejected. BT v2 already handles rendering efficiently.
- ~~**Steering vs follow-up input queuing**~~ — Implemented. Multi-turn queue, escape-to-pop, visual indicator in progress line.
- ~~**Paste markers**~~ — Implemented. Collapse large pastes (>10 lines or >1000 chars) into markers, expand on submit.
- ~~**RPC/print mode**~~ — Implemented. `--print` flag with `--prompt` or stdin pipe, auto-approves tool calls, configurable timeout.

### Patterns to defer or reject
- Full extension system — overkill for ion's scope
- Pi packages ecosystem — premature
- Cursor markers — Bubble Tea textarea handles cursor positioning
- Configuration cascade — current config is sufficient

### Ion guardrails

Pi and Claude Code are useful references, but ion should stay idiomatic for Go + Bubble Tea v2:

- translate portable ideas into `Model`/`Msg`/`Cmd` boundaries, not JS/React abstractions
- keep state split by lifecycle concern, not by source-product architecture
- prefer overlay state and sub-models over a generalized component framework
- use queued input and progress/status surfaces where they reduce blocking behavior
- only add diff rendering, extension systems, or new layers when a concrete ion need proves the complexity
- do not introduce `local-jsx`, DOM, reconciler, or package-ecosystem ideas unless the product shape changes

Practical ordering:

1. stabilize the inline single-agent loop
2. finish the runtime primitives that make that path reliable
3. build subagent runtime semantics
4. add inline subagent presentation
5. defer alternate-screen swarm orchestration until the previous layers are solid

Product ladder:

1. solo agent
2. dependent runtime capabilities
3. orchestration wrappers

## Current technical debt worth tracking

- `tk-ekao` — provider-by-provider auth and fetch validation
- `tk-lmhg` — define real skill support instead of conflating it with instruction files
- `tk-5t72` — dual storage in `CantoBackend`
- `tk-9n7h` — reevaluate `internal/backend/registry`
- `tk-st4q` — clean headless ACP-agent mode
