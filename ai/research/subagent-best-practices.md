# Subagent Best Practices (Opus 4.6+)

Findings from auditing Claude Code's agent/skill setup. Apply when ion implements its own subagent/review/refactor workflows.

## Key Principles

**1 reviewer, not N.** One agent with the full checklist (correctness + safety + quality) beats multiple narrow-focus agents reading the same code. Splitting by review type (correctness vs safety vs quality) causes redundant reads and requires deduplication. If parallelism is needed, split by **file area** (module A vs module B), not by focus.

**Analyze in main first.** Refactoring analysis, debugging, and small reviews (<50 lines) don't need subagents. Spawn only for genuine context isolation (fresh eyes on a review) or parallelism (independent subsystems).

**Build once in parent.** Run tests before spawning any agent. Include output in the agent prompt. Never let multiple agents build the same project — they contend on the build lock.

**Read-only reviewers.** Reviewers find issues, they don't fix them. Don't give review agents edit/write tools for source code. Write only for persisting findings.

**No trampoline skills.** A skill that only spawns a subagent adds nothing — just use the agent directly. Skills should add value: baseline measurement, scope detection, checklist enrichment.

**Designer threshold.** Don't auto-spawn a designer/architect agent for routine multi-file changes (renames, interface updates). Reserve for genuine architecture: new module boundaries, dependency restructuring, type hierarchy redesign.

## What Changed with Opus 4.6

- Long-context retrieval: 18.5% → 76% (MRCR v2) — model handles larger diffs in one pass
- Root cause analysis: +30% — better at diagnosing issues without narrow focus
- ARC-AGI-2: 37.6% → 68.8% — much stronger reasoning, less need for focus-splitting
- 128K output tokens — can produce comprehensive reviews without truncation

The narrow-focus multi-agent pattern was a workaround for weaker models. With current reasoning capability, one agent with all lenses is more effective than N agents with narrow focus on the same code.

## Applied Changes (chezmoi dotfiles, Feb 2026)

| File                | Change                                                    |
| ------------------- | --------------------------------------------------------- |
| `/review` skill     | 3 agents → 1 agent, added large-review guidance           |
| `/refactor` skill   | Removed auto-spawn designer for 3+ files                  |
| `/profile` skill    | Deleted (was trampoline for profiler agent)               |
| `reviewer.md` agent | Removed Edit tool (read-only reviewer)                    |
| `CLAUDE.md`         | Compact threshold ~100k → ~150k, added teams vs subagents |
