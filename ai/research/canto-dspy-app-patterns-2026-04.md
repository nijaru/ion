---
date: 2026-04-21
summary: Canto authoring and DSPy/GEPA patterns Ion should preserve for future implementation.
status: active
---

# Canto Authoring App Patterns For Ion

**Status:** active input; DSPy review (`canto-5y3y`) is one non-blocking reference
**Date:** 2026-04-21

## Purpose

Capture framework-level ideas from Canto's Phase 5 authoring work that Ion should preserve for future implementation. This is not an implementation spec yet; it is a reference note for the next Ion pass after Canto reaches a reasonably stable M1 shape. The target is a Claude Code/Codex/Cursor-class coding agent that can also safely act on external services/APIs through tools such as web/search, MCP, HTTP clients, and workflow integrations. DSPy is one inspiration here, alongside Canto's existing SOTA research, PydanticAI/LangGraph comparisons, framework-readiness review, and Ion's actual friction.

## Patterns To Preserve

| Pattern | Ion implication |
| --- | --- |
| Typed task contracts | Common Ion workflows should become named input/output contracts instead of ad hoc prompt strings. |
| Structured outputs | Planner decisions, tool plans, review findings, and summaries should be schema-validated and retryable. |
| Module/strategy split | The same task contract should be able to run through direct prediction, normal agent loop, subagent, or workflow strategy. |
| Adapter boundary | Provider quirks, structured-output formatting, and tool-call protocol differences should stay behind Canto/provider adapters, not leak into TUI code. |
| Service/API tools | Web search, MCP, HTTP/API clients, and workflow integrations should share approval, secret, audit, retry, and structured-output boundaries. |
| Trajectory capture | Ion turns, tool calls, approvals, corrections, and final outcomes should be preserved as eval/optimization data. |
| Human correction as signal | Edits, approvals, rejections, and manual fixes should be captured as high-quality examples for future prompt/example/model optimization. |

## Near-Term Use

- Use this note when running Canto's Ion friction intake.
- Do not add Ion-only framework shims; upstream durable/authoring gaps to Canto.
- Treat Canto's DSPy/GEPA reviews as eval/optimization guidance, not runtime prompt mutation.
- First concrete Ion home is eval trace capture (`tk-txju`): typed inputs, prompts/configs, tool calls, approvals, outputs, scores, and textual feedback.
- Budget/model-routing work (`tk-90mp`) should emit data that later optimizer traces can consume.

## Canto Links

- `canto-5y3y` — DSPy deep review for authoring and optimization
- `canto-87se` — GEPA review for reflective optimization and trace artifacts
- `canto-0j58` — declarative authoring surface design sketch
- `canto-p73h` — Ion consumption friction intake
- `canto-m4nb` — Ion end-to-end validation pass
