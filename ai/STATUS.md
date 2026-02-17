# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Dogfood readiness             | 2026-02-16 |
| Status    | Collapsed tool display done   | 2026-02-16 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 497 passing (`cargo test -q`) | 2026-02-16 |
| Clippy    | clean                         | 2026-02-16 |

## Completed This Session

- Collapsed-by-default tool display with Ctrl+O toggle (tk-l4oq)
  - ToolMeta on non-grouped entries enables rebuild on toggle
  - Collapsed: read/bash/list show ✓, search shows count, edit/write always inline
  - Expanded (Ctrl+O): full tail-truncated output (previous default)
  - Session replay stores ToolMeta identically to live path
  - Removed dead `load_from_messages` (superseded by lifecycle.rs)
  - 11 new tests, review-cleaned (no clone, no dead code, grammar fix)

## Known Issues

- tk-nupp (p2): Empty response observed once with chatgpt provider — trace logging active
- tk-xjmf (p3): Missing newline above progress line during streaming
- tk-86lk (p3): `--continue` header pinning breaks scrollback

## Blockers

- None.

## Next Steps

1. Manual test: collapsed tool display, Ctrl+O toggle, `--continue` replay
2. tk-43cd (p3): Persist MessageList display entries in session storage
3. tk-xjmf (p3): Missing newline above progress line
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
