# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Core agent completeness       | 2026-02-21 |
| Status    | Gemini OAuth warning complete | 2026-02-21 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 511 passing (`cargo test -q`) | 2026-02-20 |
| Clippy    | clean                         | 2026-02-19 |

## Completed This Session (2026-02-21)

- **Gemini OAuth ban warning** (tk-3vog) — active Google ban wave confirmed (Feb 2026).
  Red `⚠ violates ToS` label in provider list. Confirm dialog (cyan borders, red danger text,
  yellow question) before switching to Gemini OAuth. ChatGPT stays yellow `⚠ unofficial`.
  `SelectorItem.warning_color` field for per-provider severity. `Mode::OAuthConfirm` added.

## Completed Previous Session (2026-02-20)

- **Chat rendering enhancements** — strikethrough, task list checkboxes, display_width consistency, visual token bar.
- **TUI render layout bugs** (tk-3yus) — wrap width, selector column headers, status line display_width.
- **// skill commands** (tk-9tig) — `//` prefix for skills, `/` for builtins. Skill completer, argument substitution.

## Blockers

- tk-cmhy blocked by tk-oh88 (config depends on sandbox landing first)

## Roadmap (priority order)

### Tier 1 — Core Agent Completeness

| Task    | Title                               | Notes                                  |
| ------- | ----------------------------------- | -------------------------------------- |
| tk-7ide | Context compaction                  | TOP PRIORITY — sessions die without it |
| tk-gzag | Mid-turn steering                   | Redirect agent while working           |
| tk-1dle | Sub-agent execution                 | Context isolation for subtasks         |
| tk-oh88 | OS sandbox execution                | Safety, unblocks config                |
| tk-btlv | Image input support                 | Screenshot paste into chat             |
| tk-43cd | Persist MessageList display entries | Session continuity QoL                 |
| tk-ltyy | ask_user tool                       | Agent-initiated clarification          |

### Tier 2 — Extensibility

| Task    | Title                   | Notes                              |
| ------- | ----------------------- | ---------------------------------- |
| tk-xhl5 | Plugin/extension system | Hooks, tools, skills, distribution |
| tk-71bb | ! bash passthrough mode | Quick shell escape                 |
| tk-vdjk | Session branching       | Git-like conversation forks        |
| tk-t861 | Shareable sessions      | Export/share conversations         |

### Tier 3 — Later

- tk-5j06: Semantic memory (mem0/Letta style) — after core is feature complete + stable
- STT: LLM input sufficient for now

## Key References

| Topic                    | Location                                            |
| ------------------------ | --------------------------------------------------- |
| Feature gap analysis     | `ai/research/feature-gap-analysis-2026-02.md`       |
| Coding agents survey     | `ai/research/coding-agents-state-2026-02.md`        |
| Compaction techniques    | `ai/research/compaction-techniques-2026.md`         |
| Codex CLI analysis       | `ai/research/codex-cli-system-prompt-tools-2026.md` |
| Tool architecture survey | `ai/research/tool-architecture-survey-2026-02.md`   |
| TUI render review        | `ai/review/tui-render-layout-review-2026-02-20.md`  |
| TUI v3 architecture      | `ai/design/tui-v3-architecture-2026-02.md`          |
| Config system design     | `ai/design/config-system.md`                        |
