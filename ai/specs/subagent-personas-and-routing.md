# Subagent Personas And Routing

## Current Slice

Ion now registers a `subagent` tool in the native Canto backend. The tool
delegates a focused task through Canto child sessions and returns the child
summary to the parent loop.

Built-in personas:

| Persona | Model slot | Tool scope | Purpose |
|---|---|---|---|
| `explorer` | `fast` | read/search/memory recall | cheap isolated context gathering |
| `reviewer` | `primary` | read/search/verify | correctness and regression review |
| `worker` | `primary` | edit/shell/verify plus read/search | scoped implementation |

Custom personas load from global Markdown files with YAML frontmatter:

```markdown
---
name: scout
description: Quick read-only repo scouting.
model: fast
tools: [read, grep, glob, list]
---
Find relevant files and summarize concrete findings with paths.
```

Default directory: `~/.ion/agents`
Config override: `subagents_path`

## Routing Policy

- `primary` uses the active provider/model.
- `fast` uses the existing fast preset resolver (`fast_model`, then provider
  catalog heuristic).
- Unknown or invalid persona files fail startup instead of silently changing
  delegation behavior.
- Tool scope is fail-closed through Canto `Registry.Subset`.

## Product Boundary

This intentionally stays small. Ion should not grow many specialized personas
by default; generic explorer/reviewer/worker cover the useful split without
forcing a complex delegation UI. More advanced swarms, worktrees, and async
operator views stay downstream of the reliable inline solo loop.
