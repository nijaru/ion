# ion Status

## Current State

| Metric    | Value                              | Updated    |
| --------- | ---------------------------------- | ---------- |
| Phase     | crates/tui Phase 6 complete        | 2026-02-23 |
| Status    | TUI lib done; ion integration next | 2026-02-23 |
| Toolchain | stable                             | 2026-01-22 |
| Tests     | 566 passing                        | 2026-02-23 |
| Clippy    | clean                              | 2026-02-22 |

## Implemented Features

| Feature             | Status            | Location                                                                                      |
| ------------------- | ----------------- | --------------------------------------------------------------------------------------------- |
| Core tools          | Done              | read, write, edit, bash, glob, grep, list, ast_grep                                           |
| Web tools           | Done              | web_fetch, web_search (built-in, default)                                                     |
| Multi-provider      | Done              | Anthropic, Google, Groq, Kimi, Ollama, OpenAI, OpenRouter                                     |
| OAuth               | Done              | Gemini CLI, ChatGPT (with ban warning for Gemini)                                             |
| Context compaction  | Done              | 3-tier: truncate → remove → LLM summarize; auto + `/compact`                                  |
| Sub-agents          | Done              | `spawn_subagent` tool + `SubagentRegistry` + YAML config; sync only                           |
| Hooks               | Done              | Pre/post tool execution; `CommandHook` (shell); config-driven via `ion.toml`                  |
| Mid-turn steering   | Done              | `message_queue` wired TUI → agent; drains between turns                                       |
| Image input         | Done              | File attachment works (png/jpg/gif/webp) via `@path`                                          |
| Config system       | Done              | `~/.config/ion/ion.toml`; hierarchical user+project; API keys, hooks, MCP, permissions        |
| Session persistence | Done              | SQLite; `--continue` resumes; completion summary saved                                        |
| Skills              | Done              | `//skill-name` completer; `$ARGUMENTS` substitution                                           |
| MCP client          | Done              | stdio + HTTP transports; tools callable by LLM                                                |
| Read/Write modes    | Done              | Shift+Tab toggle; path sandbox (CWD enforcement)                                              |
| Token tracking      | Done              | Bar in status line; per-turn usage; cost tracking                                             |
| Bash passthrough    | Done              | `! cmd` prefix runs shell command directly                                                    |
| Configurable status | Done              | TOML flags: show_model, show_tokens, show_cost, show_branch, show_git_diff                    |
| Auto-backtick paste | Done              | `auto_backtick_paste = true` in config wraps multi-line pastes                                |
| Session export      | Done              | `/export` writes markdown to working dir                                                      |
| ast-grep tool       | Done              | Structural code search via `sg` binary                                                        |
| ask_user tool       | Done              | Agent pauses and asks user a question; TUI intercepts Enter to respond                        |
| crates/tui library  | Done (Phases 1–6) | Cell buffer, Taffy layout, App+Effect, Input/List/Block/Canvas/Theme; ion integration pending |

## Open Backlog (p4 only)

| Task    | Title                   | Notes                                             |
| ------- | ----------------------- | ------------------------------------------------- |
| tk-xhl5 | Plugin/extension system | Defer — premature without users/plugins           |
| tk-vru7 | colgrep evaluation      | Research: semantic code search as external tool   |
| tk-r11l | Agent config locations  | Research: standard paths for agent configs/skills |
| tk-nyqq | Symlink agents/skills   | Chezmoi dotfile task, not a code change           |
| tk-4gm9 | Settings selector UI    | Needs design doc first                            |

## Provider Expansion — Current State

**`llm` crate (graniet/llm v1.3.7):** Passes streaming + incremental tool calls. Blocked on
Anthropic system prompt `cache_control` — not implemented. Tool-level cache_control merged
2026-02-20 but not released. Watch for v1.4.0. See `ai/research/provider-crates-fresh-2026-02.md`.

**`genai` verdict:** Still out — tool calls accumulated+emitted at end (not incremental), issue #60 unresolved.

## Key References

| Topic                    | Location                                                |
| ------------------------ | ------------------------------------------------------- |
| Provider crate research  | `ai/research/provider-crates-fresh-2026-02.md` (latest) |
| Feature gap analysis     | `ai/research/feature-gap-analysis-2026-02.md`           |
| Coding agents survey     | `ai/research/coding-agents-state-2026-02.md`            |
| Compaction techniques    | `ai/research/compaction-techniques-2026.md`             |
| Tool architecture survey | `ai/research/tool-architecture-survey-2026-02.md`       |
| TUI render review        | `ai/review/tui-render-layout-review-2026-02-20.md`      |
| Config system design     | `ai/design/config-system.md`                            |
