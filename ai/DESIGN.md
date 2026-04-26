# ion Design

Updated: 2026-04-26

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
| **3** | **Framework** | `canto` (Agent loop, prompt pipeline, approval/safety primitives, Context Governor, Session Log) |
| **2** | **Logic** | `llm` (Provider abstraction, Token counting, Cost calculation) |
| **1** | **Transport** | `http` (API clients, JSON-RPC, SSE) |

## SOTA & Minimalist Goals

`ion` aims for SOTA (State of the Art) capabilities with a minimalist, terminal-first UX. This is driven by 14 core SOTA product requirements mapped to the layers above:

- **Safety by Default & Guardrails (SOTA 9):** Ion owns the user-facing READ/EDIT/YOLO policy engine and TUI approval bridge; Canto provides reusable approval/safety primitives. Includes LLM-as-a-Judge for future auto-mode safety checks.
- **Infinite Context & Compaction (SOTA 6):** Managed by a background "Context Governor" in `canto` (Layer 3). Runtime turns auto-recover from context overflow. Requires non-blocking Compaction UX indicators (spinning icons) and summarization prompts targeting fast models (Haiku/Flash).
- **Subagent Spawning & Orchestration (SOTA 7):** First-class support for child agents via `canto` primitives. Requires defined Agent Personas ("Scout", "Guard", "Build") and Model Routing policies (Explore = fast, Build = premium).
- **Memory & Knowledge Base (SOTA 1):** Karpathy-style knowledge base (Wiki compilation) and QMD-style search UX, with background consolidation using sleep-time compute.
- **Session Durability (SOTA 3):** TUI Branching (visual session rewind, `git log --graph` style), global `/search`, and Cross-Host Sync UX.
- **MCP Extensibility & Tools (SOTA 2):** Dynamic Tool Loading UX for `search_tools` (when >20 tools exist) and Approval Tier UX mapping to permission models.

Important constraint:
- Pi and Claude Code are useful maturity targets and references (see cross-pollination), but not hard feature-parity gates.
- Advanced orchestration work is downstream of a stable, feature-complete single-agent inline loop.
- The solo agent is the core product; subagents, ACP, and swarm views are wrappers around that core.

## TUI architecture

### Modular Component Design

The TUI is being refactored into isolated sub-models to improve maintainability and testing, now incorporating SOTA UX requirements:
- **`Viewport`:** Purely for rendering the committed terminal scrollback, including inline Subagent Plane B presentation.
- **`Input`:** Manages the textarea, history, and status line (which must reflect Compaction UX and Cost Limit / Reasoning Budgets per SOTA 14).
- **`Broker`:** A headless component managing the backend connection and event translation.
- **`UX Streaming` (SOTA 5):** Smoothly reconciles Canto's `iter.Seq2` into Bubbletea, handling transcript verbosity for tools and reasoning.

## Runtime/session boundary

The TUI consumes backend interfaces. That separation is real inside the repo.

Current limitation:

- ion is not yet a clean reusable external runtime library
- bootstrap/orchestration still lives mainly in `cmd/ion/main.go`
- host/runtime contracts still depend on ion-owned `internal/` packages

Session metadata now carries a cheap `Title` plus a one-line `Summary`; the resume picker uses `Title -> Summary -> LastPreview` and treats `LastPreview` as fallback, not the primary label.

Current Canto integration boundary:

- Ion depends on Canto `f47e7de` or newer current surface.
- Request processors use `canto/prompt` (`prompt.RequestProcessor`, `prompt.MemoryPrompt`).
- Hooks use `hook.Handler` and `hook.FromFunc`.
- Ion deliberately owns product tools in `internal/backend/canto/tools`: shell, file read/write/edit/list, grep, glob, verify, compact, and the host approval request bridge.
- Canto does not provide default grep/glob tools or preset coding-tool bundles; those should remain Ion product choices unless a concrete reusable extension package is designed later.

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

Auth and Model guidance (SOTA 14):

- most providers should stay simple API-key or custom-endpoint entries
- subscription/OAuth providers should be explicit and provider-specific
- CLI-bridge providers stay separate from native API providers
- ChatGPT subscription support, if we ever add it, should be treated as a separate evaluation track rather than assumed to be part of the native API path
- **Model Cascades:** The policy determining when to fall back to a cheaper model (e.g., Flash/Haiku) based on task complexity must be integrated into the provider abstraction.

## Prompt and instruction layering

Keep these distinct:

1. ion core system prompt
2. runtime/session context
3. repo-local instruction files (`AGENTS.md`, `CLAUDE.md`)
4. **Marketplace Skills (SOTA 8):** First-class runtime/TUI skill integration (`ion skill install`). Includes Self-Extension Nudges within system prompts to use `manage_skill`, and Trust Policies for secure directories.
5. task/mode reminders

## Pi-mono cross-pollination

Pi-mono analysis complete (see `research/pi-architecture.md`, `design/cross-pollination.md`). These guardrails align cleanly with the minimalist SOTA UX requirements:

### TUI improvements to adopt
- ~~**Bounding-box diff rendering**~~ — Rejected. BT v2 already handles rendering efficiently.
- ~~**Steering vs follow-up input queuing**~~ — Implemented. Multi-turn queue, escape-to-pop, visual indicator in progress line.
- ~~**Paste markers**~~ — Implemented. Collapse large pastes (>10 lines or >1000 chars) into markers, expand on submit.
- ~~**RPC/print mode**~~ — Implemented. `--print` flag with `--prompt` or stdin pipe, auto-approves tool calls, configurable timeout.

### Patterns to defer or reject
- Full extension system — overkill for ion's scope (we will rely on Marketplace Skills and MCP instead)
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
- `tk-lmhg` — (Superseded by SOTA 8 `tk-g78q`) define real skill support instead of conflating it with instruction files
- `tk-5t72` — dual storage in `CantoBackend`
- `tk-9n7h` — reevaluate `internal/backend/registry`
- `tk-st4q` — clean headless ACP-agent mode
