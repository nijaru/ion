# Design Specs

Architectural specifications for ion.

## Active

| Spec                                                              | Purpose                                                  |
| ----------------------------------------------------------------- | -------------------------------------------------------- |
| [Runtime Stack Plan](./runtime-stack-integration-plan-2026-02.md) | rmcp/rnk/genai integration roadmap (reviewed 2026-02-11) |
| [TUI v3 Architecture](./tui-v3-architecture-2026-02.md)           | Render pipeline, frame planning, width safety            |
| [Chat Soft-Wrap + Viewport Separation](./chat-softwrap-scrollback-2026-02.md) | Append-only chat + ephemeral bottom UI on resize |
| [Dogfood Readiness](./dogfood-readiness-2026-02.md)               | Sprint 16-18 roadmap                                     |
| [Permissions v2](./permissions-v2.md)                             | Read/Write modes, sandbox                                |
| [Agent & Tools](./agent.md)                                       | Multi-turn loop, context, skills                         |
| [Compaction v2](./compaction-v2.md)                               | Context summarization design                             |

## Core Reference

| Spec                                            | Purpose                         |
| ----------------------------------------------- | ------------------------------- |
| [TUI v2](./tui-v2.md)                           | Layout, crossterm, render model |
| [TUI Render Pipeline](./tui-render-pipeline.md) | Detailed render pipeline spec   |
| [Chat Positioning](./chat-positioning.md)       | insert_before algorithm         |
| [Config System](./config-system.md)             | TOML layering, MCP              |
| [Session Storage](./session-storage.md)         | SQLite persistence              |
| [Tool Pass](./tool-pass.md)                     | Bash/grep enhancements          |
| [OAuth Subscriptions](./oauth-subscriptions.md) | ChatGPT/Gemini OAuth            |

## Historical

| Spec                                            | Purpose               |
| ----------------------------------------------- | --------------------- |
| [Keybindings](./keybindings.md)                 | Key mapping reference |
| [Module Structure](./module-structure.md)       | File organization     |
| [Diff Highlighting](./diff-highlighting.md)     | Edit tool display     |
| [Interrupt Handling](./interrupt-handling.md)   | Cancellation flow     |
| [Dependency Upgrades](./dependency-upgrades.md) | Crate update tracking |
| [Model Listing](./model-listing-refactor.md)    | Registry refactor     |
| [Plugin Architecture](./plugin-architecture.md) | Future plugin design  |
