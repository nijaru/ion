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

- **Selector column headers + gap fix** — `SELECTOR_OVERHEAD=7` was claiming 1 extra row that was
  never rendered (comment always said "list header"). Added `column_header: Option<(String,String)>`
  to `SelectorData`; `render_list` now paints a dim header row using that slot. Gap bug fixed
  (2-line → 1-line). (`src/tui/render/selector.rs`, `src/tui/render/direct.rs`)

- **Model selector columns** — now shows Org, context window, and Price/M (in/out) columns.
  Added `format_price` / `format_price_pair` helpers. Provider picker shows ID/Auth columns.
  Session picker shows Directory column. (`src/tui/util.rs`, `src/tui/render/direct.rs`)

- **cargo update** — 64 packages updated (incl. `time`, `uuid`, `zerocopy`, `zmij`).

## Key Findings This Session

- **MCP gap (low priority)**: MCP tools are not exposed as API tool definitions — agent can
  discover via `mcp_tools` search but cannot call them (Anthropic API rejects unknown tool names).
  Infrastructure exists, fix needed when MCP becomes priority. Options: add MCP tools to
  `list_tools()`, or add `call_mcp_tool(name, args)` proxy builtin.

- **Skills**: Complete and correct. YAML + XML formats, lazy loading, registry, no external crate.
  No action needed.

- **Agent loop**: Solid. Retry (3x, exponential), stale timeout (120s), non-streaming fallback,
  parallel tools, abort token. No obvious bugs. tk-nupp was likely provider-side transient.

## Known Issues

- tk-nupp (p2): Empty response from chatgpt — likely transient, marked done, trace logging active

## Blockers

- None.

## Next Steps

1. tk-43cd (p3): Persist MessageList display entries in session storage
2. tk-ioxh (p3): Evaluate async subagent execution model
3. tk-cmhy (p3): TOML config for approved sandbox directories
4. MCP callable tools (when priority rises): expose McpManager tools as API tool definitions

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
