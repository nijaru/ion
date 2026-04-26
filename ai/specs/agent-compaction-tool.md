# Spec: Agent Self-Initiated Compaction Tool

**Task:** `tk-pw3s`, `tk-2wrb`
**Status:** Implemented tool; UX and summarizer guidance refined.

## Problem

Compaction is currently triggered by token limits or manual `/compact`. The agent has better information than any heuristic about whether context is still useful — it knows when the user has shifted topics, when a task is done and its implementation details are no longer relevant, and when fresh context would serve the next request better.

## Proposal

Expose a `compact` tool to the agent. The agent invokes it when it judges that current context won't serve the next task well.

## Tool Interface

```
compact(reason, preserve)
```

| Param     | Type     | Purpose |
|-----------|----------|---------|
| `reason`  | string   | Brief human-readable explanation shown in transcript |
| `preserve`| []string | Hints for the summary: file paths, decisions, or facts the agent wants carried forward |

The tool returns a summary of what was compacted and the approximate token savings.

## Agent Guidance

The agent should be instructed (via system prompt) to consider compacting when:

- The user shifts to a different part of the codebase or a new topic
- A task completed and the next request is unrelated
- The agent loaded large amounts of context (file reads, search results) that won't be needed again
- The agent detects it's nearing context limits and the user hasn't sent a complex message recently

The agent should **not** compact:

- Mid-tool-call (wait for the turn to settle)
- If the last compaction was very recent (guardrail)
- When the user's last message is complex enough that full context is needed to respond

## User Visibility

When the agent or host compacts, the progress line shows:

```
Compacting context...
```

The composer remains usable. New turns submitted while compaction is active are queued and sent after compaction completes, matching the normal in-flight turn queue behavior.

When compaction completes, the transcript receives a compact system notice:

```
Compacted current session context
```

No confirmation prompt — the agent makes the call. The user can see the result and judge if it was appropriate. If the user disagrees, they can say "don't compact without asking" and the agent adjusts behavior for the session.

## Guardrails

| Rule | Value |
|------|-------|
| Min turns since last compact | 2 |
| Min tokens since last compact | 10k |
| Never compact mid-tool-call | enforced |
| Never compact on the first turn | enforced |

These are ion-owned defaults, configurable via config.

## Architecture

This is an **ion-owned product surface** on top of Canto primitives.

- **Canto** provides the compaction mechanism (`Summarize`, context truncation)
- **Ion** provides the policy (the tool definition, guardrails, transcript rendering)

Implementation touches:

| Layer | Change |
|-------|--------|
| Tool registry | Register `compact` tool with the agent's tool set |
| Tool handler | Validate guardrails, call canto compaction, return summary |
| System prompt | Guidance on when to use (and not use) the tool |
| Summarizer prompt | Preserve goal, next step, paths, task IDs, commits, decisions, blockers, failures, root causes, and verification status; discard transient command noise and resolved detours |
| Transcript/status | Render compact progress and completion notice |
| Config | Guardrail thresholds |

## Novelty

No current agent (Claude Code, Codex CLI, OpenCode, pi) gives the model compaction control. They all treat it as infrastructure. This inverts that — the model is the best judge of its own context relevance.

## Open Questions

- Should the agent see its own token usage to make better decisions?
- Should `preserve` hints influence the summarizer, or just the agent's next message?
- Should there be a session-level opt-out (user says "never auto-compact")?
