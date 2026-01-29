# ion Status

## Current State

| Metric    | Value           | Updated    |
| --------- | --------------- | ---------- |
| Phase     | TUI v2 Complete | 2026-01-27 |
| Status    | Testing         | 2026-01-29 |
| Toolchain | stable          | 2026-01-22 |
| Tests     | 122 passing     | 2026-01-29 |
| Clippy    | 97 pedantic     | 2026-01-29 |

## Open Bugs

| ID      | Issue                                               | Priority |
| ------- | --------------------------------------------------- | -------- |
| tk-l9bn | Session ID printed on startup with no messages      | p2       |
| tk-7bcv | --continue resume broken                            | p2       |
| tk-7aem | Progress line duplicates on terminal tab switch     | p2       |
| tk-1lso | Kimi k2.5 errors on OpenRouter (root cause unclear) | p2       |
| tk-2bk7 | Resize clears pre-ion scrollback                    | p3       |

## Architecture Decisions Pending

### llm-connector Dependency (tk-aq7x)

**Problem:** llm-connector blocks features we need:

| Missing Feature             | Impact                                   |
| --------------------------- | ---------------------------------------- |
| OpenRouter `provider` field | Can't use ProviderPrefs for routing      |
| Anthropic `cache_control`   | No prompt caching (50-100x cost savings) |
| Custom request fields       | Can't add provider-specific params       |

**Options:**

1. Remove llm-connector, implement direct API calls (~500 LOC)
2. Fork and add missing fields
3. PR upstream and wait

ProviderPrefs already built in `src/provider/prefs.rs` but unused.

### Kimi Provider

- **Native provider works**: api.moonshot.ai with OpenAI-compatible format
- **OpenRouter errors**: Root cause unclear (tk-1lso) - NOT same as provider routing issue
- Kimi supports BOTH OpenAI and Anthropic-compatible APIs per their docs

## Module Health

| Module    | Health | Notes                     |
| --------- | ------ | ------------------------- |
| tui/      | GOOD   | v2 complete, testing      |
| agent/    | GOOD   | Clean turn loop           |
| provider/ | OK     | llm-connector limitations |
| tool/     | GOOD   | Orchestrator + spawn      |
| session/  | GOOD   | SQLite persistence + WAL  |
| skill/    | GOOD   | YAML frontmatter          |
| mcp/      | OK     | Needs tests               |

## Recent Work

**2026-01-29:**

- Fixed cursor off-by-1 (root cause: control chars in placeholder had zero display width)
- Added native Kimi provider (api.moonshot.ai, OpenAI-compatible)
- Sprint 12 clippy pedantic refactoring (139→97 warnings)
- Documented llm-connector limitations vs Kimi-specific issues (separate problems)

**2026-01-28:**

- TUI v2 polish: table rendering, markdown spacing, exit cleanup
- Investigated OpenRouter Kimi errors - built ProviderPrefs but can't send via llm-connector

## Key Files

- `ai/design/tui-v2.md` - TUI v2 architecture
- `ai/research/agent-survey.md` - Competitive analysis
- `src/provider/prefs.rs` - OpenRouter routing prefs (unused due to llm-connector)
