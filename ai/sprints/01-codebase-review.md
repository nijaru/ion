# Sprint 01: Codebase & Plan Review

**Goal:** Perform a comprehensive, fresh-eyes review of the entire ion codebase, architecture, and task plans utilizing Gemini 3.1 Pro to identify architectural flaws, technical debt, and missing coverage.

## Task 1: Review Architecture & Plans
**Status:** complete
**Depends on:** none
**Criteria:** Review `ai/DESIGN.md`, `ai/PLAN.md`, `ai/STATUS.md`, and specifications in `ai/specs/`. Flag any inconsistencies, missing definitions, or outdated assumptions.
**Findings:** `GEMINI.md` missing from instruction loader; `tools-and-modes.md` spec lags behind recently implemented read-mode policy.

## Task 2: Review Core Backend Layer
**Status:** complete
**Depends on:** Task 1
**Criteria:** Complete analysis of `internal/backend/` and `internal/providers/`. Verify framework integration, session context handling, tool dispatch, and the newly implemented Policy Engine restrictions.
**Findings:** Backend session mode initialization missing in startup; tight storage coupling between ion and Canto framework types.

## Task 3: Review Frontend TUI Layer
**Status:** complete
**Depends on:** Task 1
**Criteria:** Complete analysis of `internal/app/`. Identify issues in Bubble Tea models, layout logic, event loops, and input handling.
**Findings:** Potential I/O blocking in `Model.New`; Goldmark parser re-initialization on every render.

## Task 4: Review Tools, Storage & ACP Bridge
**Status:** complete
**Depends on:** Task 2, Task 3
**Criteria:** Review `internal/storage/`, `internal/session/`, built-in Canto tools (like Bash), and the ACP (`internal/backend/acp/`) integration. Identify edge cases in shell security and JSON-RPC bridging.
**Findings:** Resource leaks in ACP terminal management (zombie processes); performance bottlenecks in `canto_store.go` due to full session reloads.

## Task 5: Synthesize Findings & Plan Updates
**Status:** complete
**Depends on:** Task 2, Task 3, Task 4
**Criteria:** Produce a final synthesized report categorizing findings by ERROR, WARN, and NIT. Create `tk` tasks for actionable fixes and update `STATUS.md`.
**Findings:** Created tasks `tk-avxc`, `tk-tm90`, `tk-oyzb`, `tk-qbd9`, and `tk-6prx`. Updated `ai/STATUS.md` with new priorities.
