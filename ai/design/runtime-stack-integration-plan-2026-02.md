# Runtime Stack Integration Plan (2026-02)

## Objective

Stabilize Ion's highest-risk surfaces while avoiding unnecessary rewrites:

1. Reduce TUI rendering bugs through targeted improvements to the custom crossterm renderer.
2. Move MCP integration to the official Rust SDK ecosystem (`rmcp`).
3. Add a standardized request layer (`genai`) where it lowers maintenance cost for API-key providers.
4. Keep SQLite session storage; improve migration discipline without changing storage engine.

## Current Baseline

| Area      | Current State                                                                                                                        | Main Risk                                                              |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------- |
| TUI       | Direct `crossterm` renderer (`src/tui/render/direct.rs`, `src/tui/render/layout.rs`, `src/tui/render_state.rs`)                      | Layout/clear/cursor/autowrap edge cases under resize and long sessions |
| MCP       | Custom wrapper over `mcp-sdk-rs` v0.3.4 (`src/mcp/mod.rs`)                                                                           | Long-term protocol/library drift, extra maintenance                    |
| Providers | Fully custom provider layer (~8,900 LOC): Anthropic native API, OpenAI-compat (5 providers), Google/ChatGPT OAuth (`src/provider/*`) | Ongoing upkeep for API differences and model behavior                  |
| Storage   | Custom `rusqlite` store with schema versioning (`src/session/store.rs`)                                                              | Manual migration management over time                                  |

> **Note:** Ion does NOT use `llm-connector`. The CLAUDE.md reference to it is outdated. Ion previously used the `llm` crate (graniet), which was explicitly rejected 2026-01-19 because it couldn't do streaming + tools for the Google provider. Since tools are always present for a coding agent, this was a dealbreaker. See `ai/DECISIONS.md` and `ai/research/rust-llm-crates-2026.md`.

## Crate Decisions

| Crate                              | Role                                      | Decision                       | Why                                                                                                          |
| ---------------------------------- | ----------------------------------------- | ------------------------------ | ------------------------------------------------------------------------------------------------------------ |
| `rmcp`                             | MCP client/server protocol                | Integrate (near-term)          | Official MCP Rust SDK (v0.14.0, 3.4M DL), best long-term interop                                             |
| `genai`                            | Request abstraction for API-key providers | Integrate as optional adapter  | v0.5.3 now has tool call streaming; reduces OpenAI-compat boilerplate for standard providers                 |
| `rusqlite_migration` or `refinery` | SQLite migration tooling                  | Integrate (low priority)       | Keeps current storage model, reduces migration drift risk                                                    |
| `rnk`                              | TUI runtime                               | Do not adopt (spike abandoned) | 158 total downloads, single author, no production evidence; see review below                                 |
| `rig`                              | Full agent framework/orchestrator         | Do not adopt                   | 5.7K stars but too much framework; would require conforming to rig's Agent model, ~278K SLoC transitive deps |
| `ratatui`                          | Full TUI framework                        | Do not adopt                   | Viewport::Inline has broken horizontal resize (#2086); doesn't fit Ion's scrollback model                    |
| storage engine replacement         | Session backend swap                      | Do not adopt                   | No immediate product need                                                                                    |

## Review: Codex Research Findings (2026-02-11)

This plan originated from Codex research. Independent review validated most crate assessments but identified key corrections:

### TUI: No crate solves Ion's problem

Every major inline terminal agent (Claude Code, Codex CLI, pi-mono, opencode, Ion) built a custom renderer. No off-the-shelf TUI crate supports Ion's "chat history in native scrollback + positioned bottom UI" pattern.

| Crate                           | Inline Mode?               | Fits Ion?           | Issue                                                                                      |
| ------------------------------- | -------------------------- | ------------------- | ------------------------------------------------------------------------------------------ |
| `rnk`                           | Yes                        | Architecture aligns | 158 downloads, single author, unstable API (7 breaking releases in 20), no resize evidence |
| `ratatui`                       | Viewport::Inline (limited) | No                  | Fixed viewport height, horizontal resize broken (#2086), insert_before limited             |
| `r3bl_tui`                      | Partial (REPL mode)        | No                  | 56K SLoC dependency tree, REPL mode is readline-style not managed viewport                 |
| `termwiz`                       | No hybrid mode             | No                  | crossterm alternative, not higher-level; switching gains nothing                           |
| `cursive`, `tui-realm`, `rxtui` | No inline                  | No                  | Fullscreen only                                                                            |

**Conclusion:** Custom crossterm with targeted improvements (differential bottom-UI rendering, debounced resize, streaming area) is the correct path. Ion's two-mode rendering (row-tracking + scroll) is validated as correct -- Claude Code, pi-mono, and Codex all use similar patterns.

Full research: `ai/research/tui-crates-2026-02.md`

### Providers: Custom is justified, genai is a viable adapter

Ion's custom provider layer exists because no crate handles streaming + tools for all providers. This remains true. What changed since January 2026:

- **genai v0.5.3** now has full tool call streaming (`ToolCallChunk`, `ChatOptions.capture_tool_calls`). Previously noted as "Planned."
- genai covers 14+ providers, has `AuthResolver` + `ServiceTargetResolver` for custom endpoints.
- ~40-50% of Ion's provider code (OpenAI-compat request building, SSE parsing) could delegate to genai.

**What genai cannot replace:**

- Anthropic `cache_control` (no crate supports this)
- OAuth/subscription providers (ChatGPT, Gemini)
- Provider quirks (`max_tokens` vs `max_completion_tokens`, `store` field, `developer` role)
- Cache read/write token granularity in usage tracking

**Agent frameworks (rig, swarms-rs, langchain-rust):** None worth adopting. Ion's agent loop, tool orchestration, and session persistence are well-suited to its needs. rig is the most mature (5.7K stars) but is a framework requiring conformity. swarms-rs is vaporware, langchain-rust stale since Oct 2024.

Full research: `ai/research/provider-crates-2026-02.md`

### MCP: rmcp is confirmed correct

`rmcp` v0.14.0 (3.4M downloads) is the official Rust MCP SDK under `github.com/modelcontextprotocol/rust-sdk`. Migration from `mcp-sdk-rs` v0.3.4 is straightforward and the right call.

## Phased Plan

### Phase 1: MCP Migration (low blast radius)

- Task: `tk-na3u`
- Scope:
  - Replace `mcp-sdk-rs` usage in `src/mcp/mod.rs` with `rmcp` client primitives.
  - Preserve current `McpFallback` contract used by `ToolOrchestrator`.
  - Keep tool search/index behavior unchanged from user perspective.
- Exit Criteria:
  - `tools/list` and `tools/call` parity tests pass against existing MCP servers.
  - No changes required in tool prompt contract or user workflow.

### Phase 2: TUI Targeted Improvements (highest impact)

> **Changed from original:** Replaced "RNK TUI Migration Spike" with targeted custom improvements. rnk's immaturity (158 DL, single author, no production users, unstable API) makes it too risky for Ion's highest-bug-rate surface. The correct approach is to fix the bugs in the existing renderer, which is architecturally sound.

- Task: `tk-add8` (scope revised)
- Scope:
  - **Differential bottom-UI rendering:** Front/back buffer comparison for the managed area (input, progress, status). Only write changed cells. Reduces flicker on terminals without CSI 2026 support.
  - **Debounced resize:** Batch rapid SIGWINCH events (100ms window) to avoid multiple reflows.
  - **Width-safe rendering contract:** Enforce the TUI v3 architecture plan's single render authority and deterministic frame pipeline.
  - **Streaming response area:** Render streaming text in the managed area above input, below scrollback. Biggest UX gap vs Claude Code / pi-mono.
- Exit Criteria:
  - Resize/autowrap bugs from manual checklist resolved.
  - No regressions in `--continue`, `/resume`, `/clear`, narrow-width scenarios.
  - Flicker measurably reduced (fewer bytes per frame in steady state).

### Phase 3: genai Provider Adapter (controlled rollout)

- Task: `tk-wr9i`
- Scope:
  - Add a feature-gated provider backend that maps Ion `ChatRequest`/stream events to `genai`.
  - Start with API-key OpenAI-compat providers (OpenAI, Groq, OpenRouter).
  - Keep existing custom provider stack as default path.
  - genai is an implementation detail behind `LlmApi` trait; Ion canonical types at all edges.
- Explicit Non-Goals in this phase:
  - Replacing Anthropic provider (needs `cache_control` genai doesn't expose).
  - Replacing OAuth/subscription providers (`chatgpt`, `gemini`).
  - Removing provider-specific routing/quirks until parity is proven.
- Exit Criteria:
  - Equivalent tool-call streaming behavior for selected providers.
  - Equivalent token usage capture and error handling for selected providers.

### Phase 4: SQLite Migration Tooling (hygiene)

- Task: `tk-ww4t`
- Scope:
  - Introduce migration crate and codify migrations currently embedded in `src/session/store.rs`.
  - Preserve current schema and data compatibility.
- Exit Criteria:
  - Existing DB upgrades cleanly.
  - New migrations are versioned/tested without manual `PRAGMA` drift.

## Complexity Guardrails

1. Avoid simultaneous multi-surface rewrites (TUI + providers + MCP in one branch).
2. Preserve Ion canonical message/tool/session types at all boundaries.
3. Gate each integration behind clear adapter boundaries and rollback path.
4. Prefer parity-first migration; do not add new product behavior during stack transitions.

## Task Map

| Task      | Purpose                                                              | Priority |
| --------- | -------------------------------------------------------------------- | -------- |
| `tk-na3u` | MCP migration to `rmcp`                                              | p2       |
| `tk-add8` | TUI targeted improvements (differential rendering, debounced resize) | p2       |
| `tk-wr9i` | `genai` provider adapter                                             | p3       |
| `tk-ww4t` | SQLite migration tooling                                             | p4       |

## Watch List

| Crate/Project            | Why                                                               | Re-evaluate When                         |
| ------------------------ | ----------------------------------------------------------------- | ---------------------------------------- |
| `rnk`                    | Architecture aligns with Ion's model, but too immature today      | Downloads >1K/week, stable 1.0 release   |
| ratatui Viewport::Inline | If PR #1964 (dynamic height) and #2355 (resize fix) merge         | Both PRs released                        |
| `genai`                  | Primary adapter candidate; expanding provider/feature coverage    | Monthly (for cache_control, OAuth hooks) |
| `rig-core`               | Design patterns for Tool trait; MCP integration reference         | Quarterly                                |
| `claude-agent-sdk-rs`    | Claude Code integration patterns (bidirectional streaming, hooks) | Quarterly                                |
