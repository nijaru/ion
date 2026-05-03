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

This is design direction, not a new default feature. The local `bash` executor
remains the active path; remote executors, environment filtering, and secret
injection are later hardening slices.

### Environment policy

Current local bash behavior inherits the Ion process environment. Do not change
that implicitly in a cleanup refactor: common developer commands often depend
on `PATH`, language/toolchain variables, SSH agent sockets, cloud profile
selectors, and editor/runtime configuration.

Target staged policy:

1. Default `inherit` behavior remains unchanged.
2. Approval previews, startup, and `/tools` report the environment policy
   without listing values.
3. `inherit_without_provider_keys` is implemented as an explicit local-bash
   policy:

```toml
tool_env = "inherit_without_provider_keys"
```

This mode inherits the normal developer environment but strips provider
credential variable names from the provider catalog (`OPENAI_API_KEY`,
`ANTHROPIC_API_KEY`, `OPENROUTER_API_KEY`, custom `auth_env_var`, and provider
alternates). Future modes such as `minimal` and `allowlist` need a separate UX
and compatibility slice.

Project files cannot weaken the environment policy. Only user-global config can
change it.

### Tool secrets

Tool secrets are not the same as provider credentials.

Future shape:

- user-global config declares named secrets by source (`env`, keychain, or
  external command later)
- tool calls request secret names, not raw values
- Ion prompts for secret injection with source name, target tool, and scope
- injected values are registered with the privacy redactor for approval text,
  tool display, logs, and provider-visible tool results
- audit records include secret names and scopes, never secret values
- remote executors receive only requested secret names, materialized on the
  executor side when possible

Do not expose a model-visible secret field on `bash` until this policy and
redaction path are implemented.

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
