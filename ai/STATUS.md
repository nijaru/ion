# ion Status

## Current State

| Metric    | Value           | Updated    |
| --------- | --------------- | ---------- |
| Phase     | TUI v2 Complete | 2026-01-27 |
| Status    | Stabilizing     | 2026-01-29 |
| Toolchain | stable          | 2026-01-22 |
| Tests     | 128 passing     | 2026-01-29 |
| Clippy    | 97 pedantic     | 2026-01-29 |

## Top Priorities

1. **Provider layer replacement** (tk-aq7x) - Replace llm-connector with native HTTP
   - Design: `ai/design/provider-replacement.md`
   - Unblocks: Anthropic caching, OpenRouter routing, Kimi fixes
2. **Anthropic caching** (tk-268g) - 50-100x cost savings, blocked by provider work
3. **Input UX** - File/command autocomplete (tk-ik05, tk-hk6p)

## Active Work

### Provider Layer Replacement

**Decision:** Replace llm-connector with native implementations.

**Phases:**

1. HTTP foundation (SSE streaming, retry logic)
2. Anthropic native client (with cache_control)
3. OpenAI-compatible client (OpenRouter, Groq, Kimi)
4. Client refactor + remove llm-connector

See `ai/design/provider-replacement.md` for full plan.

## Open Bugs

| ID      | Issue                            | Root Cause                                   |
| ------- | -------------------------------- | -------------------------------------------- |
| tk-7aem | Progress line tab switch dupe    | Missing focus event handling (may fixed)     |
| tk-1lso | Kimi errors on OpenRouter        | llm-connector parsing (fix in provider work) |
| tk-2bk7 | Resize clears pre-ion scrollback | Needs decision on preservation strategy      |

## Fixed Today (2026-01-29)

**UI Fixes:**

- tk-990b: Input border in scrollback (anchor cleared prematurely in event handler)
- tk-ei0n: Selector rendering in --continue sessions
- tk-5cs9: Ctrl+D in selector
- tk-c73y: Token display per-turn
- tk-mnq0: Model name bleed into hint line
- tk-dbcr: Gap when switching selectors (reverted flicker, accept gap)
- tk-mcof: Nested list content preserved
- Startup border artifact on first message
- Word-aware wrapping for narrow tables
- Selector remnants cleared on exit before first message

**Agent Fixes:**

- Retry delays now interruptible (Esc cancels immediately)

**Docs:**

- DESIGN.md updated for TUI v2 (crossterm, not ratatui)

## Module Health

| Module    | Health   | Notes                    |
| --------- | -------- | ------------------------ |
| tui/      | GOOD     | v2 complete, stabilizing |
| agent/    | GOOD     | Clean turn loop          |
| provider/ | REFACTOR | Replacing llm-connector  |
| tool/     | GOOD     | Orchestrator + spawn     |
| session/  | GOOD     | SQLite persistence + WAL |
| skill/    | GOOD     | YAML frontmatter         |
| mcp/      | OK       | Needs tests              |

## Key References

| Topic                | Location                           |
| -------------------- | ---------------------------------- |
| TUI architecture     | ai/design/tui-v2.md                |
| Provider replacement | ai/design/provider-replacement.md  |
| Plugin design        | ai/design/plugin-architecture.md   |
| OpenRouter prefs     | src/provider/prefs.rs (unused yet) |
| Competitive analysis | ai/research/agent-survey.md        |
