# ion System Design

## Overview

`ion` is now a Go-first rewrite centered on an inline terminal host built with Bubble Tea v2.

The architecture is organized around a single host-facing session boundary so the UI can drive:

- a native ion agent runtime
- ACP-backed external agents
- later direct API-native execution paths behind the same abstraction

Historical Rust implementation details are archived under `archive/rust/`.

## System Shape

```text
User
  -> Go Host (Bubble Tea)
  -> AgentSession interface
      -> NativeIonSession
      -> ACPAgentSession
  -> Transcript / tools / approvals / plans / persistence
```

## Active Components

| Component | Responsibility |
| --- | --- |
| `go-host/` | Inline terminal host, transcript, composer, footer, event loop |
| `AgentSession` boundary | Typed lifecycle and event model for any backend |
| `NativeIonSession` | Native ion runtime using direct APIs, tools, memory, and orchestration |
| `ACPAgentSession` | External agent bridge for Claude Code, Gemini CLI, and similar systems |
| `ai/` | Durable design memory for the rewrite |
| `archive/rust/` | Historical implementation and reference docs |

## Core Design Principles

- **One host, many agent backends**: the UI should not know whether it is talking to a native ion runtime or an external ACP-backed agent.
- **ACP-shaped, not ACP-wire-native**: session and event types should align with ACP concepts without making raw protocol structs the app’s domain model.
- **Transcript-first UX**: transcript, composer, footer, progress, tools, and approvals are the product core.
- **Host/runtime separation**: Bubble Tea state is UI state; agent/session state lives behind the session boundary.
- **Preserve reusable research**: memory, context, subagents, swarms, RLM, tools, and storage remain first-class future work.

## Current Implementation Priorities

1. Finalize the host shell behavior.
2. Replace the fake backend with the canonical session interface.
3. Implement the first native ion backend in Go.
4. Add ACP-backed external agents.
5. Add persistence, memory/context, and higher-order agent orchestration.
