# ion Status

## Current State

| Metric    | Value                                             | Updated    |
| --------- | ------------------------------------------------- | ---------- |
| Phase     | Core agent completeness                           | 2026-02-22 |
| Status    | crates/tui/ Phase 1+2 done; Phase 3 (layout) next | 2026-02-22 |
| Toolchain | stable                                            | 2026-01-22 |
| Tests     | 594 passing (561 ion + 33 tui)                    | 2026-02-22 |
| Clippy    | clean                                             | 2026-02-22 |

## Implemented Features

| Feature             | Status | Location                                                                               |
| ------------------- | ------ | -------------------------------------------------------------------------------------- |
| Core tools          | Done   | read, write, edit, bash, glob, grep, list                                              |
| Web tools           | Done   | web_fetch, web_search (built-in, default)                                              |
| Multi-provider      | Done   | Anthropic, Google, Groq, Kimi, Ollama, OpenAI, OpenRouter                              |
| OAuth               | Done   | Gemini CLI, ChatGPT (with ban warning for Gemini)                                      |
| Context compaction  | Done   | 3-tier: truncate → remove → LLM summarize; auto + `/compact`                           |
| Sub-agents          | Done   | `spawn_subagent` tool + `SubagentRegistry` + YAML config; sync only                    |
| Hooks               | Done   | Pre/post tool execution; `CommandHook` (shell); config-driven via `ion.toml`           |
| Mid-turn steering   | Done   | `message_queue` wired TUI → agent; drains between turns                                |
| Image input         | Done   | File attachment works (png/jpg/gif/webp) via `@path`; no clipboard paste needed        |
| Config system       | Done   | `~/.config/ion/ion.toml`; hierarchical user+project; API keys, hooks, MCP, permissions |
| Session persistence | Done   | SQLite; `--continue` resumes; completion summary saved                                 |
| Skills              | Done   | `//skill-name` completer; `$ARGUMENTS` substitution                                    |
| MCP client          | Done   | stdio + HTTP transports; tools callable by LLM                                         |
| Read/Write modes    | Done   | Shift+Tab toggle; path sandbox (CWD enforcement)                                       |
| Token tracking      | Done   | Bar in status line; per-turn usage; cost tracking                                      |

## Open Tasks (by priority)

| Task    | Pri | Title                               | Notes                                                                         |
| ------- | --- | ----------------------------------- | ----------------------------------------------------------------------------- |
| tk-8v8x | p3  | crates/tui/ Phase 3                 | Taffy layout, Text widget, Block (borders), Row/Col; expand Element with tree |
| tk-43cd | p3  | Persist MessageList display entries | Session continuity — blank history on `--continue`                            |
| tk-ioxh | p3  | Parallel subagent execution         | Parallel tool calls + parallel subagent dispatch                              |
| tk-71bb | p4  | ! bash passthrough mode             | ~20 lines; pi has it                                                          |
| tk-ww4t | p4  | SQLite migrations                   | Schema changes silently break sessions without this                           |
| tk-ltyy | p4  | ask_user tool                       | Agent-initiated clarification; channel infra exists                           |
| tk-3fm2 | p4  | DeepSeek cache token fields         | Bug: wrong field names break cost tracking                                    |
| tk-n3q8 | p4  | read: offset/limit allocates all    | Bug: O(n) alloc for 50-line read of 10k-line file                             |
| tk-9zri | p4  | Auto-backticks on paste             | ~20 lines; pi has it                                                          |
| tk-q82k | p4  | Configurable status line            | TOML show/hide flags; not an extension API                                    |
| tk-xhl5 | p4  | Plugin distribution                 | Defer — premature without users/plugins                                       |

See `tk ls` for full backlog (deferred: tk-t861, tk-vru7, tk-r11l, tk-nyqq).

## Provider Expansion (tk-o0g7) — Current State

**Strategy:** Add common providers natively. Don't route users through OpenRouter.

| Path                             | Scope                                                     | Status                                               |
| -------------------------------- | --------------------------------------------------------- | ---------------------------------------------------- |
| OpenAI-compat base_url config    | xAI, Mistral, Together, Fireworks, Perplexity, local vLLM | Not started                                          |
| Native: DeepSeek                 | Quirks already partially handled                          | Fix tk-3fm2 first                                    |
| Native: Cohere, Bedrock, Mistral | New protocol paths                                        | Not started                                          |
| `llm` crate adapter              | Replace openai_compat/ (~4k lines → ~200-line adapter)    | Blocked: no system prompt cache_control in crate yet |

**`llm` crate (graniet/llm v1.3.7) verdict:** Passes streaming + incremental tool calls + text+tools coexistence (verified from source, has named test). Blocked on Anthropic system prompt `cache_control` — not implemented. Tool-level cache_control merged 2026-02-20 but not released. Watch for v1.4.0. See `ai/research/provider-crates-fresh-2026-02.md`.

**`genai` verdict:** Still out — tool calls accumulated+emitted at end (not incremental), issue #60 unresolved.

## Recent Completed (2026-02-22)

- **crates/tui/ Phase 1** — geometry, style, cell buffer, terminal backend. 28 unit tests.
  Cargo workspace set up with ion as root member + `crates/tui/` crate.
- **crates/tui/ Phase 2** — event system (crossterm translation), App+Effect trait, tokio select!
  event loop, Canvas widget, hello world example. 594 tests total (561 ion + 33 tui).
- **TUI cleanup (steps 1–8)** — ion-specific refactor complete. rnk removed. `ansi` module,
  `text` module, split chat renderer, explicit composer width, row-string buffer + snapshot tests.
- **TUI lib spec** — Saved to `ai/design/tui-lib-spec.md`. Defines `crates/tui/` general-purpose
  library: cell buffer, Taffy layout, App+Effect, widget tree.
- **Provider crate research** — `llm` passes streaming/tool use. Blocked on system prompt
  `cache_control` for full migration. Watch v1.4.0. See `ai/research/provider-crates-fresh-2026-02.md`.
- **Backlog audit** — Cut 4 over-engineered tasks. OS sandbox closed won't-do. See DECISIONS.md.

## Key References

| Topic                    | Location                                                |
| ------------------------ | ------------------------------------------------------- |
| Provider crate research  | `ai/research/provider-crates-fresh-2026-02.md` (latest) |
| Provider crate survey    | `ai/research/provider-crates-2026-02.md`                |
| Feature gap analysis     | `ai/research/feature-gap-analysis-2026-02.md`           |
| Coding agents survey     | `ai/research/coding-agents-state-2026-02.md`            |
| Compaction techniques    | `ai/research/compaction-techniques-2026.md`             |
| Tool architecture survey | `ai/research/tool-architecture-survey-2026-02.md`       |
| TUI render review        | `ai/review/tui-render-layout-review-2026-02-20.md`      |
| Config system design     | `ai/design/config-system.md`                            |
