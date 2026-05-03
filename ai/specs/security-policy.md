# Security Policy Config

## Current Slice

Ion owns deterministic tool policy configuration. Canto provides enforcement
hooks and tool execution primitives; Ion maps tools to user-facing risk
categories and decides whether a request is allowed, denied, or prompted.

Policy is not sandboxing. A tool may be approved by policy and still fail
closed at the executor/sandbox boundary.

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

1. AUTO mode allows everything.
2. READ mode is a hard boundary:
   - read tools allow
   - write and execute tools deny
   - sensitive non-read tools are hidden from provider requests and denied or
     prompted only if reached through an explicit host path
3. EDIT mode:
   - read tools allow
   - exact tool rule applies
   - category rule applies
   - unknown or unset tools ask

Read tools are intentionally ungated. This preserves the core loop invariant
that READ and EDIT can always inspect the workspace without approval churn.

## Executor And Secrets Boundary

Tool execution should flow through one executor boundary:

```text
policy decision -> executor -> sandbox -> local process or remote job
```

Security responsibilities:

- provider API keys stay with provider clients and are not tool environment
  variables by default
- subprocess/remote-job credentials require explicit secret injection with
  source, target, and scope visible to Ion
- approval previews and transcript/tool display must redact injected secret
  values
- executor startup failures fail before a tool call is recorded as running
- cancellation kills the process group or remote job handle owned by that
  executor
- remote sandbox providers must return bounded stdout/stderr and an exit status
  through the same tool-result shape as local execution

This is design direction, not a new default feature. The current local `bash`
implementation remains the active path until an executor refactor is scheduled.

## Deferred Work

LLM-as-judge remains behind deterministic policy config. The current foundation
adds an optional `PolicyClassifier` hook only for EDIT-mode decisions that would
already require `ask`; it cannot weaken READ mode, read-tool access, explicit
allows, or explicit denies.

Classifier circuit breakers are part of the boundary:

- timeout or model unavailability -> `ask`
- invalid/parse-failed action -> `ask`
- hard-boundary disagreement -> classifier is not called
- every classifier outcome/fallback can emit a `PolicyAuditEvent`

Real model-backed adapters are still deferred. Before enabling one by default,
Ion needs a concrete audit sink wired into durable session/trace storage and a
small prompt/schema that returns only `allow`, `ask`, or `deny` with a reason.

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

Implemented deterministic surfaces:

- approval descriptions and args shown in the TUI
- approval notification text
- live tool preview titles
- ACP headless tool start raw input/title, streamed tool-output display, and
  tool-result display sent to external ACP hosts
- ACP stderr debug logs when `ION_ACP_STDERR_LOG` is enabled

Provider-visible prompt/history redaction is not enabled by default. Redacting
the actual prompt or tool observation can change the task the model is solving.
Future prompt/export/cache privacy should therefore be explicit, scoped, and
auditable rather than silently mutating core agent history.
