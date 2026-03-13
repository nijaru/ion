# Charm Crush (Go) Architecture Research

*Date: 2026-03-13*

## Overview
[Charm Crush](https://github.com/charmbracelet/crush) is a high-performance agentic TUI built in Go by the Charm team. It is a direct reference for `ion` because it uses the same core stack (Go, Bubble Tea, Lip Gloss).

## Architecture & UX
- **Service-Oriented Core:** Clean separation of concerns into internal packages: `agent` (coordinator/tools), `services` (session, message, history, db), `integration` (LSP, shell, permission), and `ui` (chat, diff, styles).
- **Bubble Tea Native:** Uses the standard Model-Update-View pattern with modular `Bubbles` for components like chat view, diffing, list navigation, and modal dialogs.
- **LSP Integration:** Live diagnostics and codebase signals are fed directly into the agent’s planning loop, improving tool selection.
- **Animations & Aesthetics:** Leverages Lip Gloss for "cute" but professional terminal aesthetics and smooth animations.

## Context & Memory
- **SQLite Persistence:** Sessions are first-class citizens stored in a SQL-backed service, including titles, token usage, and costs.
- **Durable AGENTS.md:** Automatically generates an `AGENTS.md` file at initialization to store project-specific instructions and cache workflow context.
- **Event-Driven UI:** The app layer subscribes to pubsub events from services (messages, tool calls, MCP) and translates them into Bubble Tea messages for the UI.

## Best-in-Class Takeaways for Ion
1. **PubSub Service Model:** Decouple the agent runtime (running in goroutines) from the UI via a robust pubsub event system.
2. **LSP Diagnostics:** Feed real-time lint/type errors into the agent's turn context to help it self-correct.
3. **Multi-Model Flexibility:** Support mid-session provider/model switching for cost and latency optimization.
4. **Permissions Gating:** Implement a robust user-permission dialog for sensitive tool calls (like `bash`).
