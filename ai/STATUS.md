# ion Status

## Current State

| Metric    | Value           | Updated    |
| --------- | --------------- | ---------- |
| Phase     | TUI v2 Complete | 2026-01-27 |
| Status    | Stabilizing     | 2026-01-29 |
| Toolchain | stable          | 2026-01-22 |
| Tests     | 128 passing     | 2026-01-29 |
| Clippy    | clean           | 2026-01-29 |

## Top Priorities

1. **Provider layer replacement** (tk-aq7x) - Replace llm-connector with native HTTP
   - Design: `ai/design/provider-replacement.md`
   - Unblocks: Anthropic caching, OpenRouter routing, Kimi fixes
2. **Anthropic caching** (tk-268g) - 50-100x cost savings, blocked by provider work
3. **App struct decomposition** - Extract TaskState, AgentContext from App

## Recent Work

### RenderState Refactor (2026-01-29)

Extracted render state from App struct, implemented row-tracking chat positioning.

**Changes:**

- New `src/tui/render_state.rs` - centralized render state
- 4 reset methods: `reset_for_reflow`, `reset_for_new_conversation`, `reset_for_session_load`, `mark_reflow_complete`
- Two-mode positioning: row-tracking (content fits) vs scroll (overflow)

**Commits:** bb36486, a10d57f

**Fixed:** tk-zn6h (chat positioning - no empty lines before short chat)

**Review findings for future:**

- App struct still large (30+ fields) - consider extracting TaskState, AgentContext
- render.rs is 800 lines - consider splitting selector rendering
- Duplicate resize handling in main.rs

## Open Bugs

| ID      | Issue                            | Root Cause                                   |
| ------- | -------------------------------- | -------------------------------------------- |
| tk-7aem | Progress line tab switch dupe    | Missing focus event handling                 |
| tk-1lso | Kimi errors on OpenRouter        | llm-connector parsing (fix in provider work) |
| tk-2bk7 | Resize clears pre-ion scrollback | Needs decision on preservation strategy      |

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

| Topic                | Location                          |
| -------------------- | --------------------------------- |
| TUI architecture     | ai/design/tui-v2.md               |
| Chat positioning     | ai/design/chat-positioning.md     |
| Provider replacement | ai/design/provider-replacement.md |
| Plugin design        | ai/design/plugin-architecture.md  |
| Competitive analysis | ai/research/agent-survey.md       |
