# ion Status

## Current State

| Metric    | Value                                                              | Updated    |
| --------- | ------------------------------------------------------------------ | ---------- |
| Phase     | TUI parity — IonApp functionally complete                          | 2026-02-23 |
| Status    | All gaps closed: pickers, completers, history, help, submit, blobs | 2026-02-23 |
| Toolchain | stable                                                             | 2026-01-22 |
| Tests     | 516 passing                                                        | 2026-02-23 |
| Clippy    | clean (zero new warnings)                                          | 2026-02-23 |
| Blocker   | None — parity gaps closed                                          | 2026-02-23 |

## What Works in IonApp (crates/tui)

- App launches, shows conversation + status bar + input
- Cursor visible in input widget
- Ctrl+C double-tap quit, Esc cancel, Shift+Tab mode toggle
- Slash commands (/clear, /compact, /model, /provider, etc.)
- Paste events handled (bracketed paste + blob storage for large pastes)
- Mouse scroll, PageUp/PageDown
- System messages rendered (dim italic)
- Tool result sync (content-length delta detection)
- Auto-scroll re-enable on scroll-to-bottom
- Terminal Drop guard (panic safety)
- Picker rendering + key routing (model/provider/session selectors)
- File/command completers wired with intercept before InputState
- Input history via inner (DB persistence, Up/Down/Ctrl+N/Ctrl+P)
- Submit flow: input normalization, history persistence
- Help overlay (Ctrl+H, ? when empty)
- History search modal (Ctrl+R with query/navigation)
- OAuth confirm dialog for Gemini
- Tool expansion resync (Ctrl+O rebuilds conversation)
- Startup header pushed to conversation on init
- Conditional Ctrl+P (provider picker vs prev_history when running)

## Remaining Polish (not blocking parity)

| Gap                    | Impact | Notes                                       |
| ---------------------- | ------ | ------------------------------------------- |
| Editor open (Ctrl+G)   | P3     | Sets flag but nothing reads it              |
| Thinking level display | P3     | Ctrl+T cycles but no visual indicator       |
| ask_user visual prompt | P2     | Text works but no distinct visual treatment |

## Implemented Features

| Feature             | Status            | Location                                                                                   |
| ------------------- | ----------------- | ------------------------------------------------------------------------------------------ |
| Core tools          | Done              | read, write, edit, bash, glob, grep, list, ast_grep                                        |
| Web tools           | Done              | web_fetch, web_search (built-in, default)                                                  |
| Multi-provider      | Done              | Anthropic, Google, Groq, Kimi, Ollama, OpenAI, OpenRouter                                  |
| OAuth               | Done              | Gemini CLI, ChatGPT (with ban warning for Gemini)                                          |
| Context compaction  | Done              | 3-tier: truncate → remove → LLM summarize; auto + `/compact`                               |
| Sub-agents          | Done              | `spawn_subagent` tool + `SubagentRegistry` + YAML config; sync only                        |
| Hooks               | Done              | Pre/post tool execution; `CommandHook` (shell); config-driven via `ion.toml`               |
| Mid-turn steering   | Done              | `message_queue` wired TUI → agent; drains between turns                                    |
| Image input         | Done              | File attachment works (png/jpg/gif/webp) via `@path`                                       |
| Config system       | Done              | `~/.config/ion/ion.toml`; hierarchical user+project; API keys, hooks, MCP, permissions     |
| Session persistence | Done              | SQLite; `--continue` resumes; completion summary saved                                     |
| Skills              | Done              | `//skill-name` completer; `$ARGUMENTS` substitution                                        |
| MCP client          | Done              | stdio + HTTP transports; tools callable by LLM                                             |
| Read/Write modes    | Done              | Shift+Tab toggle; path sandbox (CWD enforcement)                                           |
| Token tracking      | Done              | Bar in status line; per-turn usage; cost tracking                                          |
| Bash passthrough    | Done              | `! cmd` prefix runs shell command directly                                                 |
| Configurable status | Done              | TOML flags: show_model, show_tokens, show_cost, show_branch, show_git_diff                 |
| Auto-backtick paste | Done              | `auto_backtick_paste = true` in config wraps multi-line pastes                             |
| Session export      | Done              | `/export` writes markdown to working dir                                                   |
| ast-grep tool       | Done              | Structural code search via `sg` binary                                                     |
| ask_user tool       | Done              | Agent pauses and asks user a question; TUI intercepts Enter to respond                     |
| crates/tui library  | Done (all phases) | Cell buffer, Taffy layout, App+Effect, Input/List/Block/Canvas/Theme; ion wired via IonApp |
| ion TUI integration | Done              | IonApp functionally matches old App — all pickers/completers/history/overlays wired        |

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
