# Canto Memory Boundary For Ion

**Date:** 2026-03-29

## Answer First

Ion should own memory product behavior.

Canto should own memory primitives and orchestration hooks.

## Ion Should Be In Charge Of

- deciding what facts are worth remembering
- deciding when to auto-remember versus ask/confirm
- choosing what counts as user core memory versus transient session memory
- memory editing/review UX
- memory visibility and inspection UX
- product-specific recall weighting and prompt behavior
- any autonomous maintenance loops or background consolidation policies

## Canto Should Be In Charge Of

- namespace/scope primitives
- memory roles
- session vs long-term memory separation
- read/write interfaces
- manager/orchestration hooks
- retrieval/write policy hooks
- durable storage implementations
- eval harnesses

## Current Guidance

Use Canto’s memory layer as the substrate:
- `session` for short-term working memory
- `memory` for long-term/core/semantic/episodic memory

Keep Ion-specific behavior above that:
- promotion rules
- “pin this” / “remember this” UX
- approval UX
- product-specific ranking

## Why

This matches the current framework/product split seen across agent systems:
- frameworks usually provide scoped memory primitives and orchestration
- products decide how aggressive, visible, and opinionated memory behavior should be

## References

- `/Users/nick/github/nijaru/canto/ai/research/memory-crosswalk-canto-ion-2026-03-29.md`
