# Subagents

Subagents are a deferred P2 surface during native-loop stabilization. They are not
part of Ion's default P1 native tool surface, and the model should not assume a
`subagent` tool is available during core-loop stabilization.

In full mode, Ion exposes a `subagent` tool for focused delegation. The default
personas are:

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
