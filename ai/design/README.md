# Design Specs

Architectural specifications for ion.

## Core

| Spec                                        | Purpose                          |
| ------------------------------------------- | -------------------------------- |
| [Agent & Tools](./agent.md)                 | Multi-turn loop, context, skills |
| [TUI v2](./tui-v2.md)                       | Layout, crossterm, render        |
| [Permission System](./permission-system.md) | Tool modes, sandbox (v1)         |
| [Permissions v2](./permissions-v2.md)       | Read/Auto modes, OS sandbox, ext |
| [Config System](./config-system.md)         | TOML layering, MCP               |
| [Session Storage](./session-storage.md)     | SQLite persistence               |
| [Tool Pass](./tool-pass.md)                 | Bash/grep enhancements           |

## Reference

| Spec                                            | Purpose                 |
| ----------------------------------------------- | ----------------------- |
| [Keybindings](./keybindings.md)                 | Key mapping reference   |
| [Chat Positioning](./chat-positioning.md)       | insert_before algorithm |
| [Module Structure](./module-structure.md)       | File organization       |
| [Diff Highlighting](./diff-highlighting.md)     | Edit tool display       |
| [Interrupt Handling](./interrupt-handling.md)   | Cancellation flow       |
| [Dependency Upgrades](./dependency-upgrades.md) | Crate update tracking   |
| [Model Listing](./model-listing-refactor.md)    | Registry refactor       |
| [OAuth Subscriptions](./oauth-subscriptions.md) | ChatGPT/Gemini OAuth    |
| [Plugin Architecture](./plugin-architecture.md) | Future plugin design    |
