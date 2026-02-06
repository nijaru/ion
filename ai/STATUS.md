# ion Status

## Current State

| Metric    | Value               | Updated    |
| --------- | ------------------- | ---------- |
| Phase     | Core hardening      | 2026-02-06 |
| Status    | Prioritized         | 2026-02-06 |
| Toolchain | stable              | 2026-01-22 |
| Tests     | 325 passing         | 2026-02-06 |
| Clippy    | clean               | 2026-02-06 |
| TUI Lines | ~9,500 (excl tests) | 2026-02-04 |

## Current Focus

Full audit complete 2026-02-06. Compared against Claude Code, Gemini CLI, aider, pi-mono.

**Verdict:** Solid architecture, feature-incomplete. Core agent loop, tools, TUI, and providers work. Main gaps are in system prompt quality, permission persistence, context management UX, and missing web search.

## Priority Queue

### P1 — Blocking agent quality

| Task    | Title                      | Why                                                    |
| ------- | -------------------------- | ------------------------------------------------------ |
| tk-zmu5 | Write proper system prompt | Current prompt is 1 sentence. No tool guidance, no CWD |

### P2 — Core functionality

| Task    | Title                                     | Why                                                  |
| ------- | ----------------------------------------- | ---------------------------------------------------- |
| tk-y4yn | Remove dead config::load_instructions     | Dead code with 4 tests, duplicates InstructionLoader |
| tk-ubad | /compact slash command                    | No way to manually trigger compaction                |
| tk-w1ou | Persist permanent tool approvals          | TODO in code; approvals lost on restart              |
| tk-yy1q | Fix Google provider (Generative Lang API) | Broken provider                                      |
| tk-g1fy | Modular streaming interface               | Needed for Google fix + robustness                   |

### P3 — Important improvements

| Task    | Title                               | Why                                |
| ------- | ----------------------------------- | ---------------------------------- |
| tk-75jw | Web search tool                     | Agents need web access             |
| tk-kxup | Cost tracking + /cost command       | All competitors show $ cost        |
| tk-i2o1 | @file/@folder inline references     | All competitors have this          |
| tk-kqie | Streaming timeout / stale detection | Hung streams freeze the UI         |
| tk-c1ij | Rate limit Retry-After parsing      | Better retry behavior              |
| tk-4fyx | Compaction threshold tuning         | 55%/45% too aggressive             |
| tk-g8xo | Session cleanup / retention         | Old sessions accumulate            |
| tk-2bk7 | Scrollback preservation on resize   | Content lost when terminal resizes |
| tk-jqe6 | Group parallel tool calls in TUI    | Visual clutter                     |
| tk-5h0j | Permission system audit             | Review for correctness             |

### P4 — Deferred

tk-ltyy, tk-epd1, tk-5j06, tk-a2s8, tk-o0g7, tk-ije3, tk-ur3b, tk-9zri, tk-4gm9, tk-tnzs, tk-imza, tk-8qwn, tk-iegz

## Competitive Comparison (2026-02-06)

### What works well (at parity or better)

| Feature          | Quality | Notes                                      |
| ---------------- | ------- | ------------------------------------------ |
| Provider count   | Best    | 9 providers (most agents have 3-5)         |
| CLI one-shot     | Best    | JSON/stream-json output, --no-tools, piped |
| Tool system      | Good    | 10 tools + MCP + hooks + permissions       |
| AGENTS.md        | Good    | 3-tier loading with mtime cache            |
| Edit tool        | Good    | String replace + replace_all + diff output |
| Permission modes | Good    | Read/Write/AGI + sandbox + bash guard      |
| Subagents        | Good    | YAML config, tool whitelist, turn limits   |
| Session resume   | Good    | SQLite WAL, picker UI                      |
| Autocomplete     | Good    | File paths + slash commands, fuzzy         |

### Critical gaps vs competitors

| Gap                | Impact    | Competitors that have it   |
| ------------------ | --------- | -------------------------- |
| System prompt      | CRITICAL  | All (extensive prompts)    |
| Web search tool    | CRITICAL  | Claude Code, aider, Gemini |
| Permission persist | CRITICAL  | Claude Code, aider         |
| /compact command   | IMPORTANT | Claude Code                |
| Cost display       | IMPORTANT | All                        |
| @file/@folder refs | IMPORTANT | Claude Code, aider, Gemini |
| Google provider    | CRITICAL  | Broken, blocks users       |

### Implementation quality

| Module        | Quality | Issues                                       |
| ------------- | ------- | -------------------------------------------- |
| agent/        | Good    | Clean loop, proper retry/cancel              |
| tool/         | Good    | Well-tested, good error handling             |
| tui/          | Good    | Clean after Sprint 14 refactor               |
| provider/     | Good    | Abstraction solid, Google needs fix          |
| config/       | Fair    | Dead code (load_instructions), no validation |
| compaction/   | Fair    | Aggressive thresholds, no manual trigger     |
| session/      | Good    | SQLite, WAL, schema migrations               |
| skill/        | Good    | Lazy load, YAML frontmatter                  |
| mcp/          | Good    | Stdio works, SSE missing                     |
| instructions  | Good    | 3-tier, cached, well-tested                  |
| system prompt | Poor    | One sentence, no tool guidance               |

## Key References

| Topic                  | Location                                |
| ---------------------- | --------------------------------------- |
| Architecture           | ai/DESIGN.md                            |
| TUI design             | ai/design/tui-v2.md                     |
| Tool pass design       | ai/design/tool-pass.md                  |
| Agent design           | ai/design/agent.md                      |
| TUI analysis           | ai/review/tui-analysis-2026-02-04.md    |
| Claude Code comparison | ai/research/claude-code-architecture.md |
