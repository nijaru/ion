# Security Policy Config

## Current Slice

Ion owns deterministic tool policy configuration. Canto provides enforcement
hooks and tool execution primitives; Ion maps tools to user-facing risk
categories and decides whether a request is allowed, denied, or prompted.

The startup loader reads a global YAML policy file:

- default: `~/.ion/policy.yaml`
- override: `policy_path` in `~/.ion/config.toml`

Schema:

```yaml
rules:
  - tool: bash
    action: deny
  - category: write
    action: allow
```

Rule selectors are mutually exclusive. `action` is one of `allow`, `ask`, or
`deny`. `category` is one of `read`, `write`, `execute`, `network`, or
`sensitive`.

## Precedence

1. YOLO mode allows everything.
2. READ mode is a hard boundary:
   - read tools allow
   - write and execute tools deny
   - sensitive tools ask
3. EDIT mode:
   - read tools allow
   - exact tool rule applies
   - category rule applies
   - unknown or unset tools ask

Read tools are intentionally ungated. This preserves the core loop invariant
that READ and EDIT can always inspect the workspace without approval churn.

## Deferred Work

LLM-as-judge remains deferred behind deterministic policy config. The likely
shape is a separate classifier policy after explicit rules and before default
`ask`, with circuit breakers for timeout, parse failure, model unavailability,
and disagreement with hard boundaries. It must fail closed to `ask` or `deny`
depending on mode/category and should emit an audit event before it can become
default behavior.

## Privacy Redaction

Prompt, trace, log, approval, and tool-preview privacy should use the same
layering as tool policy:

1. deterministic recognizers for obvious PII and secrets
2. configurable redaction/obscuring before display or export when the raw value
   is not needed
3. optional model-assisted classification only behind an explicit detector
   interface

Do not make a model call the only privacy boundary. OpenAI's public moderation
docs currently document `omni-moderation-latest` for harmful-content
classification; they do not establish a dedicated PII detector for Ion to depend
on. If a PII-specific model becomes official, evaluate it as one detector in the
pipeline, with fail-closed behavior and auditability.
