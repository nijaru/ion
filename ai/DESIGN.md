# ion System Design

## Overview

`ion` is a Go-first rewrite centered on an inline terminal host built with Bubble Tea v2.

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
  -> Storage (JSONL + SQLite)
  -> Transcript / tools / approvals / plans / persistence
```

## Active Components

| Component               | Responsibility                                                         |
| ----------------------- | ---------------------------------------------------------------------- |
| `internal/app`          | Inline terminal host, transcript, composer, footer, event loop         |
| `internal/session`      | Typed lifecycle and event model for any backend                        |
| `internal/storage`      | Persistent storage (JSONL events, SQLite indexing/input history)       |
| `internal/backend`      | Backend adapters (native, fake, ACP)                                   |
| `AgentSession` boundary | Canonical interface between host and backends                          |
| `NativeIonSession`      | Native ion runtime using direct APIs, tools, memory, and orchestration |
| `ACPAgentSession`       | External agent bridge for Claude Code, Gemini CLI, and similar systems |
| `ai/`                   | Durable design memory for the rewrite                                  |
| `archive/rust/`         | Historical implementation and reference docs                           |

## Core Design Principles

- **One host, many agent backends**: the UI should not know whether it is talking to a native ion runtime or an external ACP-backed agent.
- **Multiplexed Swarms & RLM**: The `session.Event` model supports parallel agent streams (`AgentID`) and task trees (`TraceID`).
- **Objective Function Verification**: Agents report `EventVerificationResult` (Pass/Fail + Metric) for autonomous RLM/autoresearch loops.
- **Static/Dynamic Model Listing**: Use `charm.land/catwalk` for static model metadata and Provider APIs for live availability.
- **ACP-shaped, not ACP-wire-native**: session and event types should align with ACP concepts without making raw protocol structs the app’s domain model.
- **Transcript-first UX**: transcript, composer, footer, progress, tools, and approvals are the product core.
- **The 1,000 Token Rule**: Keep system prompts and tool definitions minimal (<1k tokens) to maximize project context.
- **Tree-Based History**: Support branching and forking sessions (implemented via SQLite indexing).
- **Gather-Act-Verify**: Agents operate in explicit research, execution, and validation phases.

## Current Implementation Priorities

1. Finalize `tk-vmdl` by integrating `internal/storage` into `cmd/ion` and `internal/app`.
2. Harden the scripted backend (`tk-n1al`) to support swarm/multiplexed event testing.
3. Build the ACP backend adapter layer (`tk-mlhe`).
4. Implement the native orchestration engine (Swarms/RLM).
