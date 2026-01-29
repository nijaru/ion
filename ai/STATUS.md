# ion Status

## Current State

| Metric    | Value           | Updated    |
| --------- | --------------- | ---------- |
| Phase     | TUI v2 Complete | 2026-01-27 |
| Status    | Stabilizing     | 2026-01-29 |
| Toolchain | stable          | 2026-01-22 |
| Tests     | 122 passing     | 2026-01-29 |
| Clippy    | 97 pedantic     | 2026-01-29 |

## Top Priorities

1. **Anthropic caching** (tk-268g) - 50-100x cost savings, blocked by llm-connector
2. **llm-connector decision** (tk-aq7x) - Unblocks caching + OpenRouter routing
3. **Input UX** - File/command autocomplete (tk-ik05, tk-hk6p)

## Open Bugs

| ID      | Issue                            | Root Cause                                              |
| ------- | -------------------------------- | ------------------------------------------------------- |
| tk-7aem | Progress line tab switch dupe    | Missing focus event handling (may be fixed)             |
| tk-1lso | Kimi errors on OpenRouter        | reasoning_content field not extracted (Q6 in tui-v2.md) |
| tk-2bk7 | Resize clears pre-ion scrollback | Needs decision on preservation strategy                 |
| tk-mcof | Unordered lists show numbers     | Needs investigation - may be parser issue               |

## Architecture Decisions Pending

### llm-connector Dependency (tk-aq7x)

**Blocks:** Anthropic caching (tk-268g), OpenRouter routing, Kimi reasoning_content

| Missing Feature             | Impact                                   |
| --------------------------- | ---------------------------------------- |
| OpenRouter `provider` field | Can't use ProviderPrefs for routing      |
| Anthropic `cache_control`   | No prompt caching (50-100x cost savings) |
| Kimi `reasoning_content`    | Can't extract thinking from Kimi models  |

**Options:** Remove (~500 LOC), fork, or PR upstream

### Hooks/Plugins

- Design exists: `ai/design/plugin-architecture.md`
- Claude Code compatible protocol
- Architecture supports it, can defer - not blocking anything

## Fixed Today (2026-01-29)

- tk-l9bn: Session ID no longer printed when no messages
- tk-7bcv: --continue already working (verified)
- tk-ei0n: Selector rendering in --continue sessions (UI height calculation)
- tk-5cs9: Ctrl+D in selector (close behavior matches input mode)
- tk-c73y: Token display - show per-turn input instead of accumulating

**Reverted:** tk-5z69 tool name aliasing - should fix at prompt/definition level, not mask with aliasing

## Module Health

| Module    | Health | Notes                     |
| --------- | ------ | ------------------------- |
| tui/      | GOOD   | v2 complete, stabilizing  |
| agent/    | GOOD   | Clean turn loop           |
| provider/ | OK     | llm-connector limitations |
| tool/     | GOOD   | Orchestrator + spawn      |
| session/  | GOOD   | SQLite persistence + WAL  |
| skill/    | GOOD   | YAML frontmatter          |
| mcp/      | OK     | Needs tests               |

## Key References

| Topic                | Location                              |
| -------------------- | ------------------------------------- |
| TUI architecture     | ai/design/tui-v2.md                   |
| Plugin design        | ai/design/plugin-architecture.md      |
| Kimi reasoning issue | ai/design/tui-v2.md Q6                |
| OpenRouter prefs     | src/provider/prefs.rs (built, unused) |
| Competitive analysis | ai/research/agent-survey.md           |
