# Research Directory

Research notes for ion development. Organized by topic, newest files listed first within each section.

## TUI & Rendering

| File                                   | Date    | Content                                                               |
| -------------------------------------- | ------- | --------------------------------------------------------------------- |
| tui-crates-2026-02.md                  | 2026-02 | **Comprehensive TUI crate evaluation** (rnk, ratatui, r3bl_tui, etc.) |
| tui-hybrid-chat-libraries-2026.md      | 2026-02 | Hybrid chat TUI library patterns                                      |
| inline-tui-rendering-deep-dive-2026.md | 2026-02 | Terminal escape sequences, scrollback, resize behavior                |
| codex-ratatui-fork-analysis.md         | 2026-02 | How Codex CLI forks/uses ratatui                                      |
| tui-diffing-research.md                | 2026-01 | Differential rendering approaches                                     |
| tui-rendering-research.md              | 2026-01 | General rendering techniques                                          |
| tui-resize-streaming-research.md       | 2026-01 | Resize handling + streaming display                                   |
| tui-selectors-http-research.md         | 2026-01 | Selector widget patterns                                              |
| inline-tui-patterns-2026.md            | 2026-01 | Cross-ecosystem inline TUI patterns                                   |
| tui-state-of-art-2026.md               | 2026-01 | State of terminal UI landscape                                        |
| ratatui-vs-crossterm-v3.md             | 2026-01 | Library comparison (ratatui won't work for Ion)                       |
| inline-viewport-scrollback-2026.md     | 2026-01 | Scrollback preservation techniques                                    |
| input-research.md                      | 2026-01 | Input handling, fuzzy matching                                        |

## Agent Comparison

| File                                     | Date    | Content                                  |
| ---------------------------------------- | ------- | ---------------------------------------- |
| coding-agents-state-2026-02.md           | 2026-02 | **Comprehensive 2026 agent landscape**   |
| designer-plan-mode-evaluation-2026-02.md | 2026-02 | Plan mode patterns across agents         |
| system-prompt-effectiveness-2026-02.md   | 2026-02 | System prompt patterns and effectiveness |
| feature-gap-analysis-2026-02.md          | 2026-02 | Ion feature gaps vs competitors          |
| failure-tracking-design-2026-02.md       | 2026-02 | Error tracking across agents             |
| claude-code-system-prompt-2026.md        | 2026-02 | Claude Code system prompt analysis       |
| codex-cli-system-prompt-tools-2026.md    | 2026-02 | Codex CLI system prompt + tools          |
| codex-tui-analysis.md                    | 2026-01 | Codex CLI TUI architecture               |
| pi-mono-architecture-2026.md             | 2026-01 | Pi-Mono design philosophy                |
| pi-mono-tui-analysis.md                  | 2026-01 | Pi-Mono TUI patterns                     |
| claude-code-architecture.md              | 2026-01 | Claude Code internals                    |
| rust-tui-agent-patterns-2026.md          | 2026-01 | Rust agent implementation patterns       |

## Providers & Models

| File                              | Date    | Content                                                                                  |
| --------------------------------- | ------- | ---------------------------------------------------------------------------------------- |
| provider-crates-2026-02.md        | 2026-02 | **Comprehensive provider + agent crate evaluation** (supersedes rust-llm-crates-2026.md) |
| prompt-caching-providers-2026.md  | 2026-01 | Prompt caching comparison across providers                                               |
| model-routing-for-subagents.md    | 2026-01 | Model selection and routing strategies                                                   |
| gemini-oauth-subscription-auth.md | 2026-02 | Gemini OAuth configuration                                                               |
| oauth-implementations-2026.md     | 2026-02 | OAuth PKCE implementation patterns                                                       |

## Context & Memory

| File                          | Date    | Content                               |
| ----------------------------- | ------- | ------------------------------------- |
| compaction-techniques-2026.md | 2026-02 | Context compaction strategies         |
| context-management.md         | 2026-01 | Context window management patterns    |
| lsp-cost-benefit-2026-02.md   | 2026-02 | LSP integration cost/benefit analysis |

## Tools & Extensions

| File                               | Date    | Content                               |
| ---------------------------------- | ------- | ------------------------------------- |
| file-refs-2026.md                  | 2026-02 | File reference patterns across agents |
| edit-tool-patterns-2026.md         | 2026-01 | Edit tool designs across agents       |
| tool-display-patterns-2026.md      | 2026-01 | Tool output UX patterns               |
| extensibility-systems-2026.md      | 2026-02 | Extension/plugin/MCP patterns         |
| plugin-architectures-2026.md       | 2026-01 | Plugin system designs                 |
| working-directory-patterns-2026.md | 2026-02 | Working directory handling            |

## Configuration & Infrastructure

| File                               | Date    | Content                          |
| ---------------------------------- | ------- | -------------------------------- |
| cli-agent-config-best-practices.md | 2026-01 | Config file patterns             |
| cli-oneshot-patterns-2026.md       | 2026-01 | One-shot/headless CLI patterns   |
| permission-systems-2026.md         | 2026-02 | Permission models and sandboxing |
| session-storage-patterns-2026.md   | 2026-01 | Session persistence approaches   |
| rust-file-finder-crates.md         | 2026-01 | File finder library options      |

## Other

| File                          | Date    | Content                             |
| ----------------------------- | ------- | ----------------------------------- |
| web-search-2026.md            | 2026-02 | Web search integration patterns     |
| ddg-html-endpoint-analysis.md | 2026-02 | DuckDuckGo HTML endpoint for search |

## Superseded (kept for reference)

| File                    | Superseded By              |
| ----------------------- | -------------------------- |
| rust-llm-crates-2026.md | provider-crates-2026-02.md |
