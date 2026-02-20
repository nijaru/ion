# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-19 |
| Status    | TUI polish + provider fixes   | 2026-02-19 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 499 passing (`cargo test -q`) | 2026-02-19 |
| Clippy    | clean                         | 2026-02-19 |

## Completed This Session

- **MCP tools callable** — MCP tool defs now included in every API request via `all_tool_definitions()`.
  Added `list_definitions()` to `McpFallback` trait, `all_tool_definitions()` to `ToolOrchestrator`.
  LLM can call MCP tools directly; `mcp_tools` is now for search/discovery only.
  System prompt updated accordingly. (`src/mcp/mod.rs`, `src/tool/mod.rs`, `src/agent/stream.rs`, `src/agent/context.rs`)

- **Selector column headers + gap fix** — Added `column_header: Option<(String,String)>` to
  `SelectorData`; `render_list` now paints a dim header row using the previously-wasted overhead slot.
  2-line gap bug fixed. (`src/tui/render/selector.rs`, `src/tui/render/direct.rs`)

- **Model selector columns** — Org, Ctx, In, Out columns with dynamic widths. Org prefix stripped
  from model label (redundant with Org column). Provider/session pickers also have column headers.
  Added `format_price` helper. (`src/tui/util.rs`, `src/tui/render/direct.rs`)

- **cargo update** — 64 packages updated.

## Key Findings This Session

- **Skills**: Complete and correct. YAML + XML formats, lazy loading, registry, no external crate.
  No action needed.

- **Agent loop**: Solid. Retry (3x, exponential), stale timeout (120s), non-streaming fallback,
  parallel tools, abort token. No obvious bugs.

## Known Issues

- tk-nupp (p2): Empty response from chatgpt — likely transient, marked done, trace logging active

## Blockers

- None.

## Next Steps

1. tk-43cd (p3): Persist MessageList display entries in session storage
2. tk-ioxh (p3): Evaluate async subagent execution model
3. tk-cmhy (p3): TOML config for approved sandbox directories
4. tk-oh88 (p3): OS sandbox execution

## Key References

| Topic                    | Location                                            |
| ------------------------ | --------------------------------------------------- |
| Codex CLI analysis       | `ai/research/codex-cli-system-prompt-tools-2026.md` |
| Prompt survey (5 agents) | `ai/research/system-prompt-survey-2026-02.md`       |
| API auto-injection       | `ai/research/api-auto-injected-context-2026.md`     |
| Tool architecture survey | `ai/research/tool-architecture-survey-2026-02.md`   |
| Tool review              | `ai/review/tool-builtin-review-2026-02-14.md`       |
| LLM crate survey         | `ai/research/rust-llm-crates-survey-2026-02.md`     |
| pi-mono provider arch    | `ai/research/pi-mono-provider-architecture-2026.md` |
| TUI v3 architecture      | `ai/design/tui-v3-architecture-2026-02.md`          |
