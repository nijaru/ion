# Tool Policy

Ion loads an optional YAML policy file at startup. By default it looks for:

```text
~/.ion/policy.yaml
```

Use `policy_path` in `~/.ion/config.toml` to point at another file:

```toml
policy_path = "/Users/nick/.ion/work-policy.yaml"
```

## Schema

```yaml
rules:
  - tool: bash
    action: deny
  - category: write
    action: allow
```

Each rule selects exactly one `tool` or `category`.

Actions:

| Action | Behavior |
|---|---|
| `allow` | Permit without prompting |
| `ask` | Prompt in the TUI |
| `deny` | Block without prompting |

Categories:

| Category | Tools |
|---|---|
| `read` | `read`, `grep`, `glob`, `list`, `recall_memory`, `remember_memory`, `compact` |
| `write` | `write`, `edit`, `multi_edit` |
| `execute` | `bash` |
| `network` | Reserved for network-capable tools |
| `sensitive` | `mcp`, `subagent` |

Exact tool rules take precedence over category rules, except read tools are
always allowed in EDIT and READ mode.

## Mode Boundaries

Policy rules refine EDIT mode. They do not weaken READ mode.

| Mode | Policy effect |
|---|---|
| READ | Read tools are allowed; write and execute tools are denied; sensitive tools ask |
| EDIT | Read tools are allowed; exact tool and category rules decide the rest |
| AUTO | All tools are allowed |

Missing policy files are ignored so a default Ion install starts without setup.
