# ion Status

## Current State

| Metric    | Value                 | Updated    |
| --------- | --------------------- | ---------- |
| Phase     | Core hardening        | 2026-02-06 |
| Status    | Permissions v2 design | 2026-02-06 |
| Toolchain | stable                | 2026-01-22 |
| Tests     | 322 passing           | 2026-02-06 |
| Clippy    | clean                 | 2026-02-06 |
| TUI Lines | ~9,500 (excl tests)   | 2026-02-04 |

## Session Summary (2026-02-06)

**Done this session (continued):**

- Permissions v2 design: Remove approval system entirely, simplify to Read/Write modes
- Researched permission models across 7 agents (Claude Code, Codex, Gemini, Amp, opencode, Droid, Pi-mono)
- Researched user behavior data (UpGuard 18K config analysis, approval fatigue, YOLO adoption)
- Researched extensibility systems (MCP, hooks, skills, plugins, TS embed, WASM)
- Researched Pi-mono extension system (TypeScript extensions via jiti, tool interception)
- Consolidated 4 research files into 2: permission-systems-2026.md, extensibility-systems-2026.md
- Complete file-by-file migration plan in ai/design/permissions-v2.md
- Closed 3 obsoleted tasks (tk-w1ou, tk-mb8l, tk-5h0j)
- Created tk-vm82 (Implement permissions v2)

**Key design decisions:**

- Two modes only: Read and Write (default). No Agi mode.
- No approval prompts. Sandbox provides security.
- CLI: `--read` and `--no-sandbox` only. Drop -r, -w, -y, --agi.
- Config `deny_commands` for opt-in command blocking.
- Extensions: MCP + hooks + skills. No embedded TS/WASM runtime.
- MCP context: tool_search meta-tool for progressive disclosure.

**Previous sessions (same day):**

- System prompt: ~450 token structured prompt
- Tool pass: bash directory param, read-mode safe commands, grep output_mode, grep context lines
- Full codebase audit + competitive comparison
- ai/ file consolidation (5 stale files merged/deleted)

## Next Session

1. **tk-vm82 (P2):** Implement permissions v2 — remove approval system, simplify modes
2. **tk-ubad (P2):** /compact slash command
3. **tk-yy1q (P2):** Fix Google provider (Generative Lang API)
4. **tk-g1fy (P2):** Modular streaming interface

## Priority Queue

### P2 — Core functionality

| Task    | Title                                     | Status |
| ------- | ----------------------------------------- | ------ |
| tk-vm82 | Implement permissions v2                  | Open   |
| tk-ubad | /compact slash command                    | Open   |
| tk-yy1q | Fix Google provider (Generative Lang API) | Open   |
| tk-g1fy | Modular streaming interface               | Open   |

### P3 — Important improvements

tk-75jw (web search), tk-kxup (cost tracking), tk-i2o1 (@file refs), tk-nyqq (symlink skills), tk-r11l (research standard locations), tk-kqie (stream timeout), tk-c1ij (retry-after), tk-4fyx (compaction tuning), tk-g8xo (session cleanup), tk-2bk7 (scrollback), tk-jqe6 (parallel tool grouping)

### P4 — Deferred

tk-ltyy, tk-epd1, tk-5j06, tk-a2s8, tk-o0g7, tk-ije3

## Key References

| Topic                  | Location                                  |
| ---------------------- | ----------------------------------------- |
| Architecture           | ai/DESIGN.md                              |
| Permissions v2         | ai/design/permissions-v2.md               |
| Permission research    | ai/research/permission-systems-2026.md    |
| Extensibility research | ai/research/extensibility-systems-2026.md |
| System prompt research | ai/research/system-prompt-survey-2026.md  |
| TUI design             | ai/design/tui-v2.md                       |
| Tool pass design       | ai/design/tool-pass.md                    |
| Agent design           | ai/design/agent.md                        |
| TUI analysis           | ai/review/tui-analysis-2026-02-04.md      |
| Claude Code comparison | ai/research/claude-code-architecture.md   |
