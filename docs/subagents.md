# Subagents

Subagents are an advanced integration surface. They are not part of Ion's
default native tool surface today, and the model should not assume a `subagent`
tool is available unless a future mode explicitly exposes it.

Ion has implementation pieces for focused delegation through Canto child
sessions, but default registration is intentionally blocked until the context
contract is explicit. A child agent must know whether it receives a summary, a
full forked history snapshot, or no parent context.

The target default personas are:

| Name | Model slot | Tools | Use |
|---|---|---|---|
| `explorer` | `fast` | read/search/memory recall | Codebase scouting and context gathering |
| `reviewer` | `primary` | read/search/shell | Correctness and regression review |
| `worker` | `primary` | edit/shell plus read/search | Scoped implementation work |

The `fast` slot resolves from `fast_model` / `fast_reasoning_effort` in
`~/.ion/config.toml`. If `fast_model` is unset, Ion picks a cheap fast model
from the active provider catalog. `primary` uses the active provider/model.

## Custom Personas

Ion also loads optional global persona files from:

```text
~/.ion/agents/*.md
```

Use `subagents_path` in `~/.ion/config.toml` to point at another directory.

Each persona is Markdown with YAML frontmatter:

```markdown
---
name: scout
description: Quick read-only repo scouting.
model: fast
tools: [read, grep, glob, list]
---
Find the files relevant to the task and return concise findings with paths.
```

Custom personas override built-ins with the same name. Keep persona prompts
short and give them only the tools they need.

## Context Modes

Future `subagent` exposure should make context transfer explicit:

| Mode | Use |
| --- | --- |
| `summary` | default compact task brief plus selected parent/project summary |
| `fork` | child starts from a snapshot of the parent's provider-visible history |
| `none` | child receives only the task and persona prompt |

Forked children are snapshots. They must not mutate the parent transcript, and
they should not see parent turns submitted after spawn. The parent receives a
concise final result unless the user expands child details.

## Deferred Coordination

Pi-style subagent-to-subagent communication and Claude Code-style forked
subagents are useful references, but Ion should add them in stages:

1. explicit context modes for one synchronous child
2. compact inline child lifecycle display
3. async/background children
4. coordination or swarm views

The normal inline chat should stay conservative: no unsolicited child wakeups,
no automatic worktree creation, and no broad persona catalog by default.
