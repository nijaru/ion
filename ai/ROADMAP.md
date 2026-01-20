# ion Roadmap

## Core Identity

**ion learns and remembers across sessions using native Rust memory.**

Unlike stateless agents, ion builds knowledge over time. The budget-aware OmenDB memory system is the core differentiator.

## Phase Summary

| Phase | Status     | Focus                                                        |
| ----- | ---------- | ------------------------------------------------------------ |
| 1     | **Done**   | Foundation (Provider + Basic TUI)                            |
| 2     | **Done**   | Core Tools & Sub-Agents (read, write, designer, tree-sitter) |
| 3     | **Done**   | Memory (Native OmenDB + Hybrid Search)                       |
| 4     | **Active** | Extensibility (MCP + Skills)                                 |
| 5     | Planned    | Polish & UX (Compaction, Git, Sessions)                      |

## Phase 1: Foundation (Completed)

- [x] Rebrand to `ion`
- [x] Multi-provider trait design
- [x] OpenRouter implementation (Streaming + Tools support)
- [x] Basic Ratatui TUI skeleton
- [x] Configuration loading (TOML)
- [x] Context compaction (auto-summarization)
- [x] Session persistence via SQLite

## Phase 2: Core Tools & Sub-Agents (Completed)

- [x] Tool trait and orchestrator implementation
- [x] Permission Matrix and Mode system (Read, Write, AGI)
- [x] Built-in tools: `read`, `write`, `grep`, `glob`, `bash`
- [x] Parallel tool execution
- [x] Designer Sub-agent (Planning)
- [x] Tree-sitter symbol mapping
- [x] Unified diff view using the `similar` crate

## Phase 3: Memory (Completed)

- [x] Native OmenDB crate integration
- [x] Budget-aware context assembly logic
- [x] Hybrid search (Full-text via Tantivy + Vector via OmenDB)
- [x] Persistent message history via SQLite
- [x] Hardware-optimized local embeddings (Snowflake Arctic)

## Phase 4: Extensibility (Completed)

**Goal**: Compatibility with the broader agent ecosystem.

- [x] MCP Client implementation (`mcp-sdk-rs`)
- [x] Project-local `.mcp.json` support
- [x] SKILL.md loader (Claude Code compatible)
- [x] minijinja prompt templating

## Phase 5: Polish & UX (Active)

**Goal**: Make it a production-ready daily driver.

- [x] ContextManager extraction (decouple assembly)
- [x] Plan-Act-Verify autonomous loop
- [ ] Session save/resume logic polish
- [ ] Git integration (automatic commits/checkpoints)
- [ ] LSP integration for semantic navigation
- [ ] Sandboxing for bash execution
