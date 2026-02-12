# Runtime Stack Integration Plan (2026-02)

## Objective

Stabilize Ion's highest-risk surfaces while avoiding unnecessary rewrites:

1. Reduce TUI rendering bugs through targeted improvements to the custom crossterm renderer.
2. Prioritize core agent/API reliability work that impacts daily dogfooding.
3. Move MCP integration to `rmcp` only when MCP is part of active product workflows.
4. Keep the provider stack custom and defer `genai` until there is clear provider-expansion pressure.
5. Keep SQLite session storage; improve migration discipline without changing storage engine.

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
| `rmcp`                             | MCP client/server protocol                | Integrate (deferred)           | Official MCP Rust SDK (v0.14.0, 3.4M DL); defer until MCP is in active use                                    |
| `genai`                            | Request abstraction for API-key providers | Skip for now (watch list)      | Adds dual-path complexity and cannot replace Anthropic cache_control, OAuth flows, or key provider quirks     |
| `rusqlite_migration` or `refinery` | SQLite migration tooling                  | Integrate (low priority)       | Keeps current storage model, reduces migration drift risk                                                    |
| `rnk`                              | TUI runtime                               | Spike only (branch + kill criteria) | Architecture aligns with Ion's inline model, but maturity risk is high; evaluate before any adoption       |
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

### Providers: Custom is justified; genai is deferred

Ion's custom provider layer exists because no crate handles streaming + tools for all providers. This remains true. `genai` is useful but currently deferred:

- **genai v0.5.3** now has full tool call streaming (`ToolCallChunk`, `ChatOptions.capture_tool_calls`).
- genai covers 14+ providers and could offload some OpenAI-compat boilerplate.
- Current decision is to avoid a dual provider path until we have concrete pressure to add many new providers quickly.

**What genai cannot replace:**

- Anthropic `cache_control` (no crate supports this)
- OAuth/subscription providers (ChatGPT, Gemini)
- Provider quirks (`max_tokens` vs `max_completion_tokens`, `store` field, `developer` role)
- Cache read/write token granularity in usage tracking

**Agent frameworks (rig, swarms-rs, langchain-rust):** None worth adopting. Ion's agent loop, tool orchestration, and session persistence are well-suited to its needs. rig is the most mature (5.7K stars) but is a framework requiring conformity. swarms-rs is vaporware, langchain-rust stale since Oct 2024.

Full research: `ai/research/provider-crates-2026-02.md`

### MCP: rmcp is confirmed correct, but not current priority

`rmcp` v0.14.0 (3.4M downloads) is the official Rust MCP SDK under `github.com/modelcontextprotocol/rust-sdk`. Migration from `mcp-sdk-rs` v0.3.4 is straightforward and the right call.

## Phased Plan

### Phase 1: RNK Bottom-UI Spike (time-boxed)

- Task: `tk-add8`
- Scope:
  - Run spike on branch `codex/rnk-bottom-ui-spike`.
  - Limit RNK usage to bottom UI (`input`, `progress`, `status`); keep chat history insertion on current custom path.
  - Validate against the manual checklist and Ghostty narrow-width regressions.
  - Record integration complexity and regression delta versus current crossterm path.
- Kill Criteria:
  - Resize/autowrap regressions match or exceed current baseline.
  - RNK requires taking over chat scrollback semantics to stay stable.
  - API churn prevents a clean fork-ready integration path.
- Exit Criteria:
  - Keep/kill decision logged in `tk-add8` and reflected in `ai/STATUS.md`.
  - If killed: continue custom crossterm improvements in the same task stream.
  - If kept: open follow-up integration tasks with explicit rollout guardrails.

### Phase 2: Core Agent/API Reliability Follow-ups

- Tasks: `tk-oh88`, `tk-ts00`
- Scope:
  - `tk-oh88`: sandbox execution behavior for normal coding workflows.
  - `tk-ts00`: persist last task summary so `--continue` progress line is accurate.
- Exit Criteria:
  - Agent flow remains stable under normal dogfood loops (run/cancel/resume/restart).
  - No regressions in TUI behavior after phase 1 decision.

### Phase 3: MCP Migration (deferred until needed)

- Task: `tk-na3u`
- Scope:
  - Replace `mcp-sdk-rs` usage in `src/mcp/mod.rs` with `rmcp` client primitives.
  - Preserve current `McpFallback` contract used by `ToolOrchestrator`.
  - Keep tool search/index behavior unchanged from user perspective.
- Exit Criteria:
  - `tools/list` and `tools/call` parity tests pass against existing MCP servers.
  - No changes required in tool prompt contract or user workflow.

### Phase 4: Deferred `genai` Adapter (watch only)

- Task: `tk-wr9i` (deprioritized)
- Decision:
  - Keep task open as a watch item only.
  - Revisit when provider growth or maintenance load justifies dual-path complexity.

### Phase 5: SQLite Migration Tooling (hygiene)

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
| `tk-add8` | RNK bottom-UI spike (time-boxed, fork-ready, kill criteria)         | p2       |
| `tk-oh88` | OS sandbox execution for tool safety                                 | p2       |
| `tk-ts00` | Persist task summary for resume progress line                        | p3       |
| `tk-na3u` | MCP migration to `rmcp` (deferred)                                   | p4       |
| `tk-wr9i` | `genai` provider adapter (deferred/watch)                            | p4       |
| `tk-ww4t` | SQLite migration tooling                                             | p4       |

## Watch List

| Crate/Project            | Why                                                               | Re-evaluate When                         |
| ------------------------ | ----------------------------------------------------------------- | ---------------------------------------- |
| `rnk`                    | Architecture aligns with Ion's model, but too immature today      | Downloads >1K/week, stable 1.0 release   |
| ratatui Viewport::Inline | If PR #1964 (dynamic height) and #2355 (resize fix) merge         | Both PRs released                        |
| `genai`                  | Primary adapter candidate; expanding provider/feature coverage    | Monthly (for cache_control, OAuth hooks) |
| `rig-core`               | Design patterns for Tool trait; MCP integration reference         | Quarterly                                |
| `claude-agent-sdk-rs`    | Claude Code integration patterns (bidirectional streaming, hooks) | Quarterly                                |
