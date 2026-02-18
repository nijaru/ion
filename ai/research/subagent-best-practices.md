# Subagent & Skill Design Notes

Research from auditing Claude Code's agent/skill patterns (Feb 2026, Opus 4.6 era).

## Relevant to Ion

**Skill design — no trampolines.** Built-in skills should do real work (scoping, measurement, checklist enrichment), not just wrap a tool call or mode switch. A skill that only invokes a tool adds no value over the tool itself.

**System prompt guidance for capable models:**

- Consolidate analysis — one thorough pass beats multiple narrow passes on the same code
- Analyze inline before reaching for tools — don't shell out when reasoning suffices
- Build/test once, then work from that output rather than re-running between small changes

These are model-dependent. Capable models (Opus, Sonnet) benefit from consolidation; smaller models (Ollama locals) may need narrower focus per step.

**If ion adds orchestration/subagents:**

- One reviewer with a full checklist beats N narrow-focus reviewers on the same code. If parallelism is needed, split by file area, not by concern.
- Reviewers should be read-only — find issues, don't fix them.
- Run build/test in the parent before spawning. Never let multiple agents contend on the same build.
- Don't auto-spawn architect/designer agents for routine multi-file changes. Reserve for genuine architecture decisions.

## Context

The narrow-focus multi-agent pattern was a workaround for weaker models. Improvements in long-context retrieval (MRCR v2: 18.5% → 76%), root cause analysis (+30%), and reasoning (ARC-AGI-2: 37.6% → 68.8%) mean one agent with all lenses is now more effective than N agents with narrow focus on the same code.

## Source

Dotfiles audit — changes applied to chezmoi config (review skill consolidated 3→1 agents, removed trampoline profile skill, reviewer made read-only).
