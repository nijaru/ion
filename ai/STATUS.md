# ion Status

## Current State

| Metric    | Value                         | Updated    |
| --------- | ----------------------------- | ---------- |
| Phase     | Core agent completeness       | 2026-02-22 |
| Status    | OS sandbox evaluated + closed | 2026-02-22 |
| Toolchain | stable                        | 2026-01-22 |
| Tests     | 511 passing (`cargo test -q`) | 2026-02-20 |
| Clippy    | clean                         | 2026-02-19 |

## Implemented Features

| Feature             | Status  | Location                                                                               |
| ------------------- | ------- | -------------------------------------------------------------------------------------- |
| Core tools          | Done    | read, write, edit, bash, glob, grep, list                                              |
| Web tools           | Done    | web_fetch, web_search (built-in, default)                                              |
| Multi-provider      | Done    | Anthropic, Google, Groq, Kimi, Ollama, OpenAI, OpenRouter                              |
| OAuth               | Done    | Gemini CLI, ChatGPT (with ban warning for Gemini)                                      |
| Context compaction  | Done    | 3-tier: truncate → remove → LLM summarize; auto + `/compact`                           |
| Sub-agents          | Done    | `spawn_subagent` tool + `SubagentRegistry` + YAML config                               |
| Hooks               | Done    | Pre/post tool execution; `CommandHook` (shell); config-driven via `ion.toml`           |
| Mid-turn steering   | Done    | `message_queue` wired TUI → agent; drains between turns                                |
| Image input         | Partial | File attachment works (png/jpg/gif/webp); clipboard paste not yet                      |
| Config system       | Done    | `~/.config/ion/ion.toml`; hierarchical user+project; API keys, hooks, MCP, permissions |
| Session persistence | Done    | SQLite; `--continue` resumes; completion summary saved                                 |
| Skills              | Done    | `//skill-name` completer; `$ARGUMENTS` substitution                                    |
| MCP client          | Done    | stdio + HTTP transports; tools callable by LLM                                         |
| Read/Write modes    | Done    | Shift+Tab toggle; path sandbox (CWD enforcement)                                       |
| Token tracking      | Done    | Bar in status line; per-turn usage; cost tracking                                      |
| Async sub-agents    | Partial | Sync execution done; parallel/background not yet                                       |
| Plugin distribution | Partial | Hooks done (phase 1); distributable packages not yet                                   |

## Open Tasks (by priority)

| Task    | Pri | Title                               | Notes                                            |
| ------- | --- | ----------------------------------- | ------------------------------------------------ |
| tk-btlv | p3  | Image clipboard paste               | File attachment done; paste missing              |
| tk-43cd | p3  | Persist MessageList display entries | Session continuity QoL                           |
| tk-ioxh | p3  | Async subagent execution            | Parallel/background subagents                    |
| tk-ltyy | p4  | ask_user tool                       | Agent-initiated clarification                    |
| tk-71bb | p4  | ! bash passthrough mode             | Quick shell escape                               |
| tk-xhl5 | p4  | Plugin distribution system          | Phase 1 (hooks) done; packages/install remaining |

See `tk ls` for full p4 backlog.

## Recent Completed (2026-02-22)

- **OS sandbox (tk-oh88)** — Closed won't-do. Existing guards (analyze_command +
  check_sandbox) sufficient; OS sandbox breaks cargo/npm caches, adds Linux
  install friction. See DECISIONS.md.
- **Gemini OAuth ban warning** (tk-3vog) — red `⚠ violates ToS` label + confirm dialog.

## Key References

| Topic                    | Location                                           |
| ------------------------ | -------------------------------------------------- |
| Compaction techniques    | `ai/research/compaction-techniques-2026.md`        |
| Coding agents survey     | `ai/research/coding-agents-state-2026-02.md`       |
| Feature gap analysis     | `ai/research/feature-gap-analysis-2026-02.md`      |
| Tool architecture survey | `ai/research/tool-architecture-survey-2026-02.md`  |
| TUI render review        | `ai/review/tui-render-layout-review-2026-02-20.md` |
| TUI v3 architecture      | `ai/design/tui-v3-architecture-2026-02.md`         |
| Config system design     | `ai/design/config-system.md`                       |
