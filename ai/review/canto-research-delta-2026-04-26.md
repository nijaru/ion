---
date: 2026-04-26
summary: Delta review of recent Canto ai/ research and plans that affect Ion.
status: active
---

# Canto Research Delta For Ion

## Answer

Recent Canto research does not change the immediate Ion implementation order, but it
does sharpen the next slices:

- Start with `tk-90mp` cost limits/model cascades.
- Treat DSPy/GEPA as eval and optimizer inputs, not runtime prompt mutation.
- Keep approval classifier heuristics, tool-selection policy, shell policy, and UX in Ion.
- Re-evaluate Ion's local file/shell tools against Canto's stable `coding/`
  primitives only as a focused follow-up, not as an upstreaming task.

## Deltas From Canto

| Source | Ion impact |
| --- | --- |
| `../canto/ai/STATUS.md` | Canto pre-Ion stabilization is complete. Ion should resume product work and feed back only concrete framework friction. |
| `../canto/ai/PLAN.md` | Canto's remaining work is docs/release posture unless Ion finds framework issues. Ion owns policy, defaults, UX, model choices, and task workflow. |
| `../canto/ai/DECISIONS.md` | Approval classifiers plug in via `approval.PolicyFunc`; Ion owns shell heuristics and HITL escalation behavior. |
| `../canto/ai/research/dspy-authoring-insights-2026-04.md` | Future Ion workflows should emit typed task/eval artifacts. Do not make prompts self-mutating at runtime. |
| `../canto/ai/research/gepa-reflective-optimization-2026-04.md` | GEPA needs optimizer-readable traces: inputs, prompts/configs, session events, tool calls, scores, and textual feedback. |
| `../canto/ai/review/tool-surface-audit-2026-04.md` | Canto exposes individual `coding/` primitives and no presets. Ion can keep product tools, but old "contribute grep/glob/default bundle" tasks are stale. |

## Task Implications

- `tk-90mp` should include explicit cost-limit telemetry and model-routing
  decisions as future eval-trace fields, not just UI state.
- `tk-txju` should become the home for DSPy/GEPA-compatible trace artifacts
  and judge/metric feedback.
- `tk-j3ap` and `tk-yf7v` should use Canto's approval seams, but the classifier
  prompts, shell heuristics, escalation copy, and mode UX stay in Ion.
- The old Canto contribution tasks should be reframed as "evaluate Ion adoption
  of Canto primitives" or closed if they only describe upstreaming default
  coding-tool bundles.

## Next Recommended Slice

Proceed with `tk-90mp` first, but design the budget/cascade data model so it can
feed later eval and optimizer traces:

- configured limits and model slots
- provider/model selected for each turn
- usage and estimated cost
- stop/escalation reason
- whether a cheap/fast model was chosen, skipped, or escalated

This keeps the implementation small while preserving the DSPy/GEPA path.
