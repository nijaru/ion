# AI Memory Index

Start here, in order. Root files are intentionally short; detailed progress belongs in the linked tracker or task logs.

## Start Here

- [STATUS.md](STATUS.md) — current phase, active blocker, and next action
- [PLAN.md](PLAN.md) — core-loop stabilization gates and deferrals
- [DESIGN.md](DESIGN.md) — current Ion/Canto architecture and ownership boundaries
- [DECISIONS.md](DECISIONS.md) — stable principles plus recent decision log

## Core Loop Evidence

- [Core Loop Review Tracker](review/core-loop-review-tracker-2026-04-28.md) — active A1-A6 subsystem sequence for P1 native-loop stabilization
- [Core Minimal Agent Reset](design/core-minimal-agent-reset-2026-04-28.md) — phased reset plan: minimal core agent, stable TUI, then advanced features
- [Active-Turn Steering Contract](design/active-turn-steering-contract-2026-04-30.md) — safe boundary for queued busy input and future steering mode
- [Ion Roadmap 2026-05-01](design/roadmap-2026-05-01.md) — current next-phase roadmap: stable native agent, TUI/CLI table stakes, then deferred SOTA
- [Native Core Loop Architecture](design/native-core-loop-architecture.md) — target ownership, invariants, refactor sequence, and smoke matrix
- [Ion Native Backend Spine](design/ion-native-backend-spine-2026-04-27.md) — backend adapter turn phases, event translation, cancel/close semantics
- [Ion Display Projection](design/ion-display-projection-2026-04-27.md) — Canto effective history plus Ion display-only event projection
- [Ion App and CLI Lifecycle](design/ion-app-cli-lifecycle-2026-04-27.md) — startup, resume, slash commands, runtime switch, progress/error, and print CLI lifecycle

## Reference Research

- [Current Pi Core Loop Review](research/pi-current-core-loop-review-2026-04.md) — Pi `/tree`, loop, compaction, and UX lessons
- [Core Agent Reference Delta](research/core-agent-reference-delta-2026-04-27.md) — Pi/Codex CLI and loop deltas; reference only until Gate 2 is stable
- [Canto Research Delta](review/canto-research-delta-2026-04-26.md) — Canto ai/ findings that affect Ion sequencing
- [Core Loop AI Corpus Synthesis](review/core-loop-ai-corpus-synthesis-2026-04-27.md) — cross-repo ai/ synthesis and pre-implementation gates
- [Canto Core Loop Contract Audit](review/canto-core-loop-contract-audit-2026-04-27.md) — Canto terminal, queue, tool, and history contract audit

## Deferred Specs

These are not active until the P1 native core loop is stable.

- [Roadmap](ROADMAP.md) — broader roadmap and lower-priority sequencing
- [SOTA Requirements](SOTA-REQUIREMENTS.md) — long-term product responsibilities
- [Status and Config Spec](specs/status-and-config.md)
- [Tools and Modes Spec](specs/tools-and-modes.md)
- [Security Policy Spec](specs/security-policy.md)
- [Subagent Personas and Routing Spec](specs/subagent-personas-and-routing.md)
- [Workspace Trust and Rollback Spec](specs/workspace-trust-and-rollback.md)
- [Tool Loading and Approval Tiers Spec](specs/tool-loading-and-approval-tiers.md)
- [Memory Search and Wiki Spec](specs/memory-search-and-wiki.md)
- [Workflows and Recovery Spec](specs/workflows-and-recovery.md)
- [Evals and Regression Gates Spec](specs/evals-and-regression-gates.md)

## Historical Review

Older review docs remain useful for context but should not be updated as active trackers.

- [Core Loop Contract](review/core-loop-contract.md)
- [Core Loop Review](review/core-loop-review.md)

## User-Facing Docs

- [Observability Docs](../docs/observability/README.md)
- [Security Policy Docs](../docs/security/policy.md)
- [Subagent Docs](../docs/subagents.md)
- [Workspace Trust Docs](../docs/workspace-trust.md)
- [Tool Docs](../docs/tools.md)
- [Memory Docs](../docs/memory.md)
