# ion Roadmap

**Updated:** 2026-01-26

## Overview

ion is a fast, lightweight TUI coding agent. This roadmap organizes remaining work into phases by impact.

## Phase 1: Cost Optimization (CRITICAL)

**Goal:** 50-100x cost reduction on Anthropic via prompt caching

| Task                      | ID      | Description                                                  |
| ------------------------- | ------- | ------------------------------------------------------------ |
| Direct Anthropic client   | tk-268g | Bypass llm-connector, implement streaming with cache_control |
| Cache conversation prefix | -       | Mark all prior turns with cache_control: ephemeral           |

**Why critical:** Long sessions (100k+ context) currently pay full price every turn. With caching, pay 90% less on everything except the new delta. This is the single highest-impact improvement.

**Implementation:**

1. `src/provider/anthropic.rs` - Direct reqwest client
2. Streaming SSE parser for Anthropic format
3. cache_control on system + all prior messages
4. Keep llm-connector for other providers

## Phase 2: Vision & Input (HIGH)

**Goal:** Enable vision workflows and improve input UX

| Task                 | ID      | Description                                       |
| -------------------- | ------- | ------------------------------------------------- |
| Image attachment     | tk-80az | @image:path syntax, base64 encoding, vision check |
| File autocomplete    | tk-ik05 | @ triggers path picker with fuzzy search          |
| Command autocomplete | tk-hk6p | / for builtins, // for skills                     |

**Dependencies:** Phase 1 not required

## Phase 3: Provider Improvements (MEDIUM)

**Goal:** Better provider support and cost visibility

| Task                      | ID               | Description                                      |
| ------------------------- | ---------------- | ------------------------------------------------ |
| Direct provider interface | tk-g1fy          | Modular streaming for provider-specific features |
| OAuth for Google          | tk-f564          | Native Google auth without API key               |
| Model sorting             | tk-wj4b, tk-r9c7 | Newest first, org grouping                       |

## Phase 4: Tools (MEDIUM)

**Goal:** Expand agent capabilities

| Task       | ID      | Description                              |
| ---------- | ------- | ---------------------------------------- |
| Web search | tk-1y3g | Search tool (SerpAPI, Brave, or similar) |
| ast-grep   | tk-imza | Structural code search/transform         |

## Phase 5: Extensibility (LOW)

**Goal:** Claude Code-like hooks and customization

| Task            | ID      | Description                            |
| --------------- | ------- | -------------------------------------- |
| Hook system     | tk-iso7 | Lifecycle events for extensions        |
| True sandboxing | tk-8jtm | Container/namespace isolation for bash |
| Theme support   | tk-vsdp | Customizable colors                    |

## Phase 6: Polish (LOW)

**Goal:** Minor UX improvements

| Task                | ID      | Description                     |
| ------------------- | ------- | ------------------------------- |
| Markdown styling    | tk-v11j | Reduce inline code highlighting |
| Model display       | tk-x3zf | provider:model vs just model    |
| Provider selector   | tk-a4q5 | Show config id and display name |
| Interactive prompts | tk-kf3r | y/n confirmation flows          |

## Deferred / Research

| Task                     | ID      | Notes                      |
| ------------------------ | ------- | -------------------------- |
| Agent loop decomposition | tk-mmpr | Nice to have, not blocking |
| System prompt comparison | tk-8qwn | Research task              |
| Permission audit         | tk-5h0j | Security review            |
| OpenRouter routing modal | tk-iegz | Idea, needs research       |

## Completed (Recent)

- ✅ Web fetch tool
- ✅ Skills YAML frontmatter (agentskills.io spec)
- ✅ Progressive skill loading
- ✅ Subagent support (spawn_subagent tool)
- ✅ Thinking display ("thought for Xs")
- ✅ TUI refactor (6 modules)
- ✅ Codebase review (all critical issues fixed)

## Timeline Guidance

| Phase             | Effort   | Impact                          |
| ----------------- | -------- | ------------------------------- |
| 1 - Caching       | 1-2 days | CRITICAL - 50-100x cost savings |
| 2 - Vision/Input  | 1 day    | HIGH - new capabilities + UX    |
| 3 - Providers     | 1-2 days | MEDIUM - better model support   |
| 4 - Tools         | 1 day    | MEDIUM - expanded capabilities  |
| 5 - Extensibility | 2-3 days | LOW - power user features       |
| 6 - Polish        | ongoing  | LOW - incremental improvements  |
