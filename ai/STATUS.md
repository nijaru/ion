# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-17 |
| Status    | TUI rendering fixes done      | 2026-02-17 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 497 passing (`cargo test -q`) | 2026-02-17 |
| Clippy    | clean                         | 2026-02-16 |

## Completed This Session

- **Collapsed tool display counts** (cont'd from prev session)
  - Collapsed read/list shows line/item count, bash shows output line count from header
  - `format_result_content` singular unit uses explicit match (not fragile `trim_end_matches`)
  - Bash header scan scoped to first 3 lines
  - Doc comment updated

- **Streaming line truncation** (tk-nx0j) — root cause: `apply_chat_insert` re-queried
  `terminal::size()` inside `BeginSynchronizedUpdate` instead of using passed `term_width`.
  Lines wrapped at one width, clipped at another → only ~N columns visible. Fixed by passing
  `term_width` through to `apply_chat_insert`.

- **Search tool coloring** — `detect_syntax` was applied to `search` tool args (treating query
  as file path). Result status lines were syntax-highlighted. Removed `search` from the branch.

- **Post-streaming reflow** — `needs_reflow` set on agent completion when `streaming_carryover`
  is non-empty; triggers `FullRerender` to correct word-wrap artifacts from incremental commits.

- **Missing newline / 3-blank gap** (tk-heug) — stripped trailing blanks from scrollback output
  in `reprint_chat_scrollback`, `take_chat_inserts` (idle), and `reprint_loaded_session`.

- **Layout gap** — `PROGRESS_GAP` const (1 row) always reserved for visual separation;
  `PROGRESS_HEIGHT` conditional on `has_active_progress`. Gap present on `--continue`.

- **Review fixes** — progress_gap_rows() → const, bounded header scan, conditional reflow,
  explicit singular unit match.

## Known Issues

- tk-nupp (p2): Empty response observed once with chatgpt provider — trace logging active
- tk-86lk (p3): `--continue` header pinning breaks scrollback (gap fixed; pinning separate)

## Blockers

- None.

## Next Steps

1. tk-43cd (p3): Persist MessageList display entries in session storage
2. tk-9ozb (p3): Selector column alignment broken
3. tk-86lk (p3): `--continue` header pinning breaks scrollback
4. tk-ioxh (p3): Evaluate async subagent execution model

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
