# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-19 |
| Status    | Quick wins complete           | 2026-02-19 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 511 passing (`cargo test -q`) | 2026-02-20 |
| Clippy    | clean                         | 2026-02-19 |

## Completed This Session (2026-02-20)

- **Chat rendering enhancements** (tk-s2xv, tk-xj3g, tk-9ar3, tk-7kqq, tk-avmd) — 5 tasks complete, 511 tests passing:
  - Strikethrough (`~~text~~`) — `with_strikethrough()` builder + `Tag::Strikethrough` in markdown
  - Task list checkboxes (`- [x]` / `- [ ]`) — `ENABLE_TASKLISTS` + `Event::TaskListMarker` → `☑`/`☐`
  - `table.rs` display_width consistency — replaced `UnicodeWidthStr` with `crate::tui::util::display_width`
  - `direct.rs` display_width — selector column alignment uses `display_width` + explicit padding
  - Visual token bar — `render_token_bar` in util.rs; status line shows `██████ 45%`

- **TUI render layout bugs fixed** (tk-3yus) — wrap width mismatch in `calculate_input_height` (was width-6, now width-INPUT_MARGIN=3); selector column header uses `display_width`; status line drop-levels use `display_width` for model/project/branch/think; `scroll_to_cursor` moved inside `content_width>0` guard.

- **// skill commands** (tk-9tig) — `//` prefix opens green skill completer; `/` stays cyan builtins.
  Skills loaded at startup from `~/.agents/skills`, `~/.ion/skills`, `.ion/skills`.
  `//skill-name [args]` activates skill with `$ARGUMENTS`/`$0`/`$1` substitution.
  New frontmatter: `user-invocable`, `argument-hint`. `~/.agents/AGENTS.md` added to instruction loader.

## Completed Previous Session

- **MCP tools callable** — `all_tool_definitions()` on `ToolOrchestrator` now includes MCP tools.
  LLM can call them directly; `mcp_tools` for search only. System prompt updated.

- **Selector column headers + gap fix** — `column_header` field uses the wasted overhead slot.
  Provides Org/Ctx/In/Out columns on model picker, ID/Auth on provider, Directory on session.
  Fixed 2-line gap after selector dismissal.

- **Tool quick wins:**
  - guard: `sudo`/`doas` prefix stripped before `analyze_command`; blocked in Read mode
  - list: MAX_RESULTS=2000 cap with truncation message
  - glob: optional `path` parameter to restrict search scope

- **Persist completion summary** (tk-zqsw) — DB migration v4 adds completion columns to sessions.
  Saved after each completed task, restored on `--continue` so progress line isn't blank.

## Blockers

- tk-cmhy blocked by tk-oh88 (config depends on sandbox landing first)

## Next Steps

1. **tk-3vog** (p2): Review Gemini OAuth — antigravity key banning users; investigate Gemini CLI OAuth flow
2. **tk-43cd** (p3): Persist MessageList display entries — needs DB schema + lifecycle work
3. **tk-oh88** (p3): OS sandbox execution — main safety feature, unblocks tk-cmhy

## Key References

| Topic                       | Location                                            |
| --------------------------- | --------------------------------------------------- |
| Codex CLI analysis          | `ai/research/codex-cli-system-prompt-tools-2026.md` |
| Prompt survey (5 agents)    | `ai/research/system-prompt-survey-2026-02.md`       |
| Tool architecture survey    | `ai/research/tool-architecture-survey-2026-02.md`   |
| Tool review                 | `ai/review/tool-builtin-review-2026-02-14.md`       |
| TUI render review           | `ai/review/tui-render-layout-review-2026-02-20.md`  |
| Chat rendering enhancements | `ai/design/chat-rendering-enhancements.md`          |
| TUI v3 architecture         | `ai/design/tui-v3-architecture-2026-02.md`          |
| Config system design        | `ai/design/config-system.md`                        |
