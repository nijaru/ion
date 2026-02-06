# ion Status

## Current State

| Metric    | Value               | Updated    |
| --------- | ------------------- | ---------- |
| Phase     | Core hardening      | 2026-02-06 |
| Status    | System prompt done  | 2026-02-06 |
| Toolchain | stable              | 2026-01-22 |
| Tests     | 322 passing         | 2026-02-06 |
| Clippy    | clean               | 2026-02-06 |
| TUI Lines | ~9,500 (excl tests) | 2026-02-04 |

## Session Summary (2026-02-06)

**Done this session:**

- System prompt: replaced 1-sentence prompt with ~400 token structured prompt (identity, core principles, tool usage hierarchy, output format, safety)
- Added working directory + date to context template via `ContextManager.with_working_dir()`
- Fixed flaky `test_no_files_returns_none` (broke when global AGENTS.md was added)
- Removed unused `tempfile::TempDir` import from config tests

**Previous session (same day):**

- Tool pass: bash directory param, read-mode safe commands, grep output_mode, grep context lines
- Full codebase audit + competitive comparison vs Claude Code, Gemini CLI, aider, pi-mono
- Removed dead config::load_instructions (83 lines)
- Set up ~/.config/agents/AGENTS.md symlink -> ~/.claude/CLAUDE.md
- System prompt research: ai/research/system-prompt-survey-2026.md

## Next Session

1. **tk-ubad (P2):** /compact slash command
2. **tk-w1ou (P2):** Persist tool approvals to config
3. **tk-yy1q (P2):** Fix Google provider (Generative Lang API)
4. **tk-mb8l (P3):** -w flag should clear config auto_approve

## Priority Queue

### P2 — Core functionality

| Task    | Title                                     | Status |
| ------- | ----------------------------------------- | ------ |
| tk-ubad | /compact slash command                    | Open   |
| tk-w1ou | Persist permanent tool approvals          | Open   |
| tk-yy1q | Fix Google provider (Generative Lang API) | Open   |
| tk-g1fy | Modular streaming interface               | Open   |

### P3 — Important improvements

tk-75jw (web search), tk-kxup (cost tracking), tk-i2o1 (@file refs), tk-nyqq (symlink skills), tk-r11l (research standard locations), tk-mb8l (-w flag fix), tk-kqie (stream timeout), tk-c1ij (retry-after), tk-4fyx (compaction tuning), tk-g8xo (session cleanup), tk-2bk7 (scrollback), tk-jqe6 (parallel tool grouping), tk-5h0j (permission audit)

### P4 — Deferred

tk-ltyy, tk-epd1, tk-5j06, tk-a2s8, tk-o0g7, tk-ije3, tk-ur3b, tk-9zri, tk-4gm9, tk-tnzs, tk-imza, tk-8qwn, tk-iegz

## Key References

| Topic                  | Location                                 |
| ---------------------- | ---------------------------------------- |
| Architecture           | ai/DESIGN.md                             |
| System prompt research | ai/research/system-prompt-survey-2026.md |
| TUI design             | ai/design/tui-v2.md                      |
| Tool pass design       | ai/design/tool-pass.md                   |
| Agent design           | ai/design/agent.md                       |
| TUI analysis           | ai/review/tui-analysis-2026-02-04.md     |
| Claude Code comparison | ai/research/claude-code-architecture.md  |
