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
- **The 1,000 Token Rule**: Keep system prompts and tool definitions minimal (<1k tokens) to maximize project context.
- **Tree-Based History**: Support branching and forking sessions to allow exploring multiple solution paths.
- **Gather-Act-Verify**: Agents operate in explicit research, execution, and validation phases.

## Current Implementation Priorities

1. Finalize the host shell behavior and UX polish.
2. Implement transcript and session persistence (`tk-vmdl`).
3. Build the ACP backend adapter layer (`tk-mlhe`).
4. Add advanced memory, context, and swarm orchestration.
