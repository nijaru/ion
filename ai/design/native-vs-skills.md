# Native Features vs. Skills & Subagents

This document defines which agent capabilities should be built directly into the `ion` TUI/agent core and which should be relegated to lazy-loaded "Skills" or background "Subagents."

## Core Principles

1.  **The 1,000 Token Rule:** The base system prompt and core tool definitions must remain under 1,000 tokens to maximize available project context.
2.  **Host-Runtime Separation:** The TUI (Host) handles rendering and terminal takeover; the Runtime handles orchestration and tool execution.
3.  **Lazy Loading:** Specialized capabilities (Docker, K8s, complex refactoring) should be loaded only when the agent identifies a need.

## Capability Mapping

| Feature | Implementation | Rationale |
| --- | --- | --- |
| **Context Saver** | Native Runtime | Essential for loop stability. Must automatically sandbox large outputs to files without user intervention. |
| **Interactive Shell** | Native Host (TUI) | Requires suspending the Bubble Tea event loop and taking over stdin/stdout. Cannot be a "skill." |
| **Explore Agent** | Native Subagent | A high-priority background worker that maintains the codebase map (`MAP.md`) and symbol index. |
| **Git / GitHub** | Native Tools | Core to the developer workflow. High-performance, built-in implementations are preferred over CLI wrappers. |
| **Web Search/Fetch** | Skill / Subagent | Clunky and context-heavy. Better as an on-demand capability for the "scientist" workflow. |
| **Auth / OAuth** | Native Core | Security and credential management must be handled by the trusted core. |

## Feature Requirements

### 1. Context Saver (Guardrails)
- **Threshold:** Any tool output >5,000 chars is automatically written to `.ion/tmp/session-id/output-xxxx.log`.
- **Reference:** The agent receives a summary and a file path reference instead of the raw dump.
- **TUI:** The host renders a clickable or selectable reference to view the full log.

### 2. Interactive Shell (Takeover)
- **Mechanism:** The Runtime emits a `ShellInteractionRequest`. The Host (Bubble Tea) calls `program.ReleaseTerminal()`, executes the command via `os/exec`, and then calls `program.RestoreTerminal()`.
- **Use Case:** `git add -p`, `npm init`, or any command requiring TTY interaction.

### 3. Explore Subagent (The Eyes)
- **Role:** Runs in parallel to the main Execution agent.
- **Output:** Continuously updates a project-level grounding file (`ai/CONTEXT.md`) so the main agent always has up-to-date architectural context.
- **Inspiration:** Claude Code's "Explore" mode.

## Implementation Roadmap

1. [ ] Design `ShellInteraction` protocol between AgentSession and Host.
2. [ ] Implement Runtime-level output sandboxing (Context Saver).
3. [ ] Define the `Explore` subagent's initial grounding prompt.
