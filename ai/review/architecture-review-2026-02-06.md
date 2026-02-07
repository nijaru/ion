# ion Architecture Review

**Date**: 2026-02-06
**Scope**: Full architecture review vs Claude Code, Codex CLI, Gemini CLI, OpenCode, Pi-mono, Crush
**Codebase**: ~28K lines Rust, 314 tests, stable toolchain, Edition 2024

---

## Module-by-Module Assessment

### 1. Agent Loop (`src/agent/`, ~400 lines core)

**Design**: Classic multi-turn loop with decomposed phases: stream response, extract tool calls, execute tools in parallel, commit to history, check compaction. This follows the Claude Code / Codex CLI pattern -- intentionally simple, single-threaded message list with no state machine or DAG orchestration.

**Strengths**: Clean phase separation (stream.rs, tools.rs, context.rs each handle one concern). The `execute_turn` function is tight at ~70 lines. Tool execution uses `JoinSet` for true parallelism with ordered result collection -- better than Codex CLI's `FuturesOrdered` which doesn't allow work-stealing. Cancellation via `CancellationToken` is threaded through cleanly. The designer (plan generation) is optional and only triggers on first messages >100 chars.

**Gaps**: No explicit "continue until done" logic like Codex CLI (which auto-compacts and retries on token limit). ion's loop relies solely on the model stopping tool calls, with no mechanism to resume after hitting context limits mid-task. No provider-reported token usage integration -- ion counts tokens locally via bpe-openai, which may drift from actual usage on non-OpenAI providers. The message queue (`Arc<std::sync::Mutex<Vec<String>>>`) for inter-turn user messages uses a `std::sync::Mutex`, which is fine since locks are held briefly, but mixing sync and async mutexes is a readability hazard.

**Compared to peers**: Matches Claude Code's "simple loop" philosophy. Simpler than OpenCode's client/server split. More structured than Pi-mono's bare loop. Lacks Amp's "handoff" pattern for long-running work.

---

### 2. Provider Layer (`src/provider/`, ~3,800 lines)

**Design**: Three native HTTP protocol implementations (Anthropic Messages API, OpenAI-compatible SSE, Google Generative AI) behind a unified `LlmApi` trait. Each provider handles its own request building, streaming, and response parsing. Provider quirks (max_tokens vs max_completion_tokens, developer vs system role, reasoning_content extraction) are isolated in `openai_compat/quirks.rs`.

**Strengths**: This is one of ion's strongest modules. The decision to write native HTTP clients rather than depend on an SDK crate (after the `llm` crate experiment failed for streaming+tools) is correct -- streaming with tool calls is the primary use case for a coding agent, and no Rust crate handles this well across all providers. The `ToolBuilder` accumulator pattern for incremental JSON tool call arguments is clean. The `StreamEvent` enum provides a good normalization layer. OAuth support for ChatGPT and Gemini subscriptions via dedicated backends is a differentiator vs most open-source agents. The quirks system avoids polluting the core protocol logic.

**Gaps**: `supports_tool_streaming()` hardcodes Local and OpenRouter as non-streaming -- this should be per-model or at least configurable, since many OpenRouter models do support streaming+tools. Google provider (tk-yy1q) is noted as broken. No request/response logging or debug tracing at the HTTP level (peers use verbose logging for diagnosis). No retry-after header parsing (tk-c1ij). No prompt caching hints for Anthropic (`cache_control: ephemeral`) despite the architecture being cache-aware by design. Missing usage data from provider responses -- the `Usage` struct exists in `StreamEvent` but is never consumed by the agent loop.

**Compared to peers**: More providers than Codex CLI (OpenAI only). Competitive with OpenCode (which uses models.dev for routing). Unique OAuth subscription support (ChatGPT Plus, Gemini consumer). Lacks Crush's Catwalk auto-updating provider database.

---

### 3. Tool System (`src/tool/`, ~2,100 lines)

**Design**: `Tool` trait with `execute(args, ctx)` signature, `ToolOrchestrator` for discovery and dispatch with permission checks and hook execution. 8 built-in tools (read, write, edit, bash, glob, grep, list, web_fetch). MCP tools implement the same trait. Permission model is binary: Read mode (safe tools only) or Write mode (everything allowed). Sandbox is path-based (CWD restriction) at the app level.

**Strengths**: The `ToolOrchestrator` is well-designed: hooks run pre/post tool use, permissions check happens inline, and tool name sanitization handles model hallucinations gracefully. The `ToolContext` with `check_sandbox()` is pragmatic. The hook system (`HookRegistry` with priority ordering, `HookResult` with Replace/Skip/Abort) is extensible without being over-engineered. The permissions v2 simplification (removing the entire approval system, 870 lines deleted) was the right call -- the research showing 60% YOLO mode adoption is damning.

**Gaps**: MCP integration is basic -- all MCP tools load eagerly at startup, consuming context. The design doc describes a `tool_search` meta-tool for progressive disclosure, but it's not implemented. No lazy tool loading (Claude Code's v2.1 feature). MCP tools are all classified as `DangerLevel::Restricted` regardless of their actual capabilities. The `ToolMetadata` struct exists but is disconnected from the `Tool` trait -- metadata is never populated or used. No structured output support (returning JSON schema-validated results). No tool timeout enforcement -- only bash has cancellation support via `kill_on_drop`. The `discover` tool (semantic search) has a stub but no backend.

**Compared to peers**: Tool set matches table stakes (read/write/edit/bash/glob/grep). Missing Claude Code's `TodoWrite` and `Task` (subagent) tools. Missing OpenCode's LSP integration. Missing Gemini CLI's Google Search grounding. Pi-mono gets by with just 4 tools, but ion's grep implementation (with `grep-searcher` backend, context lines, output modes) is superior to most competitors.

---

### 4. Context Management (`src/agent/context.rs` + `src/compaction/`, ~560 lines)

**Design**: `ContextManager` assembles the system prompt from a minijinja template, combining base instructions, AGENTS.md layers, active plan, active skill, and environment metadata. `RenderCache` avoids re-rendering when plan/skill haven't changed. Compaction uses two-tier pruning: (1) truncate large tool outputs to head+tail, (2) remove old tool output content entirely with a summary placeholder. Protected messages window (last 12) prevents pruning recent context.

**Strengths**: The template-based system prompt assembly with caching is good architecture -- it ensures the system prompt prefix is stable for provider-side cache hits. The instruction loader with mtime-based caching is efficient and supports the AGENTS.md / CLAUDE.md standard. Compaction triggers at 80% of context window, targets 60% -- well within the research-recommended range.

**Gaps**: This is ion's most significant architectural gap relative to peers. There is no Tier 3 (LLM-based summarization) -- only mechanical pruning. Claude Code, Codex CLI, and OpenCode all use the LLM itself to summarize conversation history, preserving semantic content. ion's pruning can only truncate or remove tool outputs; it cannot compress reasoning, decisions, or exploration history. There is no `/compact` slash command (tk-ubad) for user-triggered compaction. No compaction summary is injected into the conversation -- once content is pruned, it's simply gone. No consideration for "what to preserve" (failed approaches, error messages, file paths) vs "what to summarize" per the research. The `memory_context` parameter in `assemble()` is plumbed but never used (OmenDB deferred). Token counting uses `bpe-openai` (cl100k_base tokenizer) which may be inaccurate for non-OpenAI models -- this affects compaction trigger accuracy.

**Compared to peers**: Behind all major agents. Claude Code does full LLM summarization. Codex CLI auto-compacts and retries. OpenCode does tiered pruning then LLM compaction. Amp abandoned compaction entirely for "handoff" (fresh context with curated state). Even Pi-mono plans to add compaction.

---

### 5. Skill System (`src/skill/`, ~590 lines)

**Design**: SKILL.md loader supporting both YAML frontmatter (agentskills.io spec) and legacy XML format. `SkillRegistry` with progressive loading -- frontmatter parsed at startup, full prompt loaded on demand. Skills inject prompts into the system prompt via the context manager template. Model constraints per skill.

**Strengths**: Excellent implementation. Progressive loading (summary-only at startup, full load on demand) is a technique Claude Code pioneered -- ion implements it correctly. Dual format support (YAML + XML) ensures compatibility with the broader agentskills.io ecosystem. The model constraint system (per-skill model whitelist) is a feature peers lack. Directory scanning for auto-discovery is clean.

**Gaps**: No skill activation via slash command (only via TUI keybinding or subagent config). No `allowed_tools` enforcement -- the field is parsed and stored but never used to filter the tool set. No concept of "disallowed tools" (Claude Code's `disallowedTools` in agent definitions). Skills don't support `@file` references for including additional context files.

**Compared to peers**: On par with Claude Code's `.claude/agents/*.prompt.md` and Crush's skills support. Better than Codex CLI and OpenCode which lack structured skill systems. The progressive loading and model constraints are novel.

---

### 6. Session Management (`src/session/`, ~660 lines)

**Design**: SQLite with WAL mode. Two tables: `sessions` (metadata) and `messages` (transcript with position ordering). Input history table for recall across sessions. Schema versioning with migration support. Sessions identified by timestamp + random suffix.

**Strengths**: WAL mode is the right choice for concurrent read access. Position-based message ordering preserves conversation structure. CTE-based batch loading is efficient. Input history (global recall) is a nice usability feature that peers lack.

**Gaps**: No session branching (Pi-mono supports this). No session sharing (OpenCode has shareable session URLs). No compaction state persistence -- if you compact and resume, the pre-compaction context is lost permanently. No session metadata beyond working_dir and model (no tags, no description, no cost tracking). No session export/import. The `SessionStore` uses synchronous rusqlite in an async context -- while fine for small operations, large session loads could block the runtime. No session retention/cleanup is implemented in the store (the 30-day retention design exists but isn't wired).

**Compared to peers**: Functional but basic. OpenCode has richer session management (multi-session parallel, sharing). Codex CLI has ghost snapshots for parallel commit tracking. Claude Code has checkpoints for state save/restore.

---

### 7. Sub-agent System (`src/agent/subagent.rs` + `src/tool/builtin/spawn_subagent.rs`)

**Design**: YAML-configurable subagent definitions with tool whitelists, optional model override, custom system prompt, and turn limits. Subagents are full `Agent` instances with isolated context and session. Results collected via event channel and returned as text.

**Strengths**: Clean isolation model -- subagents get their own `ToolOrchestrator` and `Session`, preventing context contamination. Turn limiting prevents runaway execution. The registry pattern (scan directory, load configs) is extensible.

**Gaps**: Subagents are designed but barely integrated. The `spawn_subagent` tool exists but the designer/explorer/researcher subagents mentioned in the design docs are not wired as default configurations. No background execution (Claude Code's Ctrl+B for background subagents). No conversational subagent mode (multi-turn dialogue between main and sub). No result summarization -- subagent output is dumped raw into context. Subagents cannot nest (correct) but also can't coordinate (no shared state or message passing). The `run_subagent` function doesn't emit meaningful progress events to the TUI.

**Compared to peers**: Framework exists but underutilized. Claude Code has three built-in agents (Explore, Plan, General) plus custom agents. Codex CLI has task types (Review, Compact, Undo). OpenCode has built-in agents (build, plan, general). Pi-mono deliberately avoids subagents in favor of spawning separate processes -- a simpler but effective approach.

---

### 8. Hook System (`src/hook/`, ~256 lines)

**Design**: Lifecycle hooks with `HookPoint` enum (PreToolUse, PostToolUse, OnError, OnResponse), priority-ordered execution, and `HookResult` supporting Continue/Skip/Replace/Abort. Integrated into `ToolOrchestrator.call_tool()`.

**Strengths**: Well-designed framework. The priority ordering with stable sort is correct. The `HookResult` enum covers the right set of actions. Pre and post hooks are both wired into the tool execution path. This is the foundation for Claude Code-compatible hooks (formatters, safety checks, logging).

**Gaps**: The framework exists but no hooks are registered by default. The config-driven hook registration (`[[hooks]]` in TOML with shell commands) described in the design doc is not implemented. No SessionStart, SessionEnd, PreCompact, or Stop hooks -- only tool-related hooks are wired. The `OnError` and `OnResponse` hook points are defined but never triggered in the codebase.

**Compared to peers**: Correct architectural foundation. Claude Code's hook system is config-driven and battle-tested. Crush uses hooks for skills integration. ion's implementation is dormant infrastructure.

---

### 9. Config System (`src/config/`, ~460 lines)

**Design**: TOML-based with three-layer precedence: global (~/.ion/config.toml), project (.ion/config.toml), project-local (.ion/config.local.toml, gitignored). Manual merge function for each field. Migration support for old config locations.

**Strengths**: The precedence model (project-local > project > global > defaults) is correct and matches the standard for CLI tools. Config file > env var priority for API keys (explicit intent wins) is a good decision. MCP server configs in TOML. Auto-migration from old paths. `ensure_local_gitignored()` helper prevents credential commits.

**Gaps**: The merge function is verbose and fragile -- each field must be explicitly handled, which means new fields are silently ignored until someone adds the merge logic. Figment was identified as the solution in the decisions doc but never adopted. No config validation (invalid provider names, missing required fields). No config file generation command (`ion init`). No `.ion/config.local.toml` creation flow. Provider preferences (`ProviderPrefs`) has filtering/routing capabilities but the UI doesn't expose configuration.

**Compared to peers**: Functional but manual. Claude Code uses CLAUDE.md (simpler). OpenCode has rich config with auto-detection. Crush uses standard JSON with community-managed provider database. Gemini CLI has GEMINI.md. The three-layer model is sound; the implementation needs polish.

---

## Top 5 Architectural Strengths

1. **Native provider layer**: Three protocol implementations (Anthropic, OpenAI-compat, Google) with streaming+tools as first-class. Writing native HTTP clients rather than depending on SDK crates was the correct decision for a coding agent where streaming tool output is the primary UX. OAuth subscription support (ChatGPT Plus, Gemini consumer) is unique in the open-source space.

2. **Clean agent loop**: The decomposition into stream.rs, tools.rs, context.rs keeps the core loop readable (~400 lines). JoinSet-based parallel tool execution with ordered result collection is correct. CancellationToken threading is thorough. The loop is simple enough to debug, following Claude Code's "simplicity through constraint" principle.

3. **Permission model v2**: The binary Read/Write mode with sandbox-based security (not prompt-based) is the right architecture. The research-backed decision to remove the approval system (60% YOLO adoption, 870 lines deleted) demonstrates good design judgment. The safe-command allowlist for bash in Read mode is pragmatic.

4. **Skill system with progressive loading**: Matching the agentskills.io spec with summary-only loading at startup and full load on demand. Model constraints per skill are a unique feature. Dual format support (YAML + XML) ensures ecosystem compatibility.

5. **TUI architecture (crossterm direct)**: The decision to drop ratatui for direct crossterm rendering, using terminal scrollback for chat history and cursor positioning for the bottom UI, is architecturally sound for an inline TUI. This matches Codex CLI's approach and avoids the `Viewport::Inline` limitations.

---

## Top 5 Architectural Gaps/Weaknesses

1. **No LLM-based compaction**: This is the single biggest gap. Every competitive agent (Claude Code, Codex CLI, OpenCode) uses the LLM to summarize conversation history before context overflow. ion's two-tier mechanical pruning (truncate outputs, remove old outputs) loses semantic content -- decisions, reasoning, exploration results are simply deleted. A long coding session will lose critical context. This should be P0.

2. **MCP integration is shallow**: All MCP tools load eagerly, consuming context regardless of relevance. The design doc describes `tool_search` for progressive disclosure but it's not implemented. Claude Code's lazy MCP loading and progressive tool descriptions are the benchmark. Without this, using MCP servers with many tools (Playwright: 21 tools, 13.7K tokens) makes ion impractical.

3. **Provider-reported usage not consumed**: The `Usage` struct in `StreamEvent` carries input/output/cache token counts from provider responses, but the agent loop ignores it entirely, relying instead on local bpe-openai estimation. This means compaction triggers may fire too early or too late on non-OpenAI models. Cost tracking (tk-kxup) is impossible without this data.

4. **Hook system is dormant**: The framework is well-designed but no hooks are registered by default, config-driven hook loading is not implemented, and 2 of 4 hook points are never triggered. This blocks the extensibility story (formatters on write, safety checks on bash, cost tracking, logging). Without this, hooks are dead code.

5. **Subagents designed but not integrated**: Explorer, Researcher, and Reviewer subagents are described in design docs but not wired as built-in defaults. The `spawn_subagent` tool exists as a builtin but isn't registered. Without at least an Explorer subagent (Haiku-class for fast search), complex codebase navigation requires the expensive main model for every grep/glob cycle.

---

## Comparison Table

| Dimension             | ion                         | Claude Code              | Codex CLI          | Gemini CLI      | OpenCode        | Pi-mono          | Crush         |
| --------------------- | --------------------------- | ------------------------ | ------------------ | --------------- | --------------- | ---------------- | ------------- |
| **Language**          | Rust                        | TypeScript               | Rust               | TypeScript      | TypeScript      | TypeScript       | Go            |
| **Binary size**       | ~15MB                       | ~100MB+                  | ~15MB              | ~100MB+         | ~100MB+         | ~50MB+           | ~30MB         |
| **Providers**         | 9                           | 1 (Claude)               | 1 (OpenAI)         | 1 (Gemini)      | 75+             | 15+              | 10+           |
| **OAuth login**       | ChatGPT, Gemini             | N/A                      | ChatGPT            | Google          | Claude, ChatGPT | No               | No            |
| **Streaming+tools**   | Native (3 protocols)        | Native                   | Native             | Native          | Via SDK         | Via SDK          | Via SDK       |
| **Tool count**        | 8 built-in                  | 8 built-in               | 6 built-in         | 7 built-in      | 8+ built-in     | 4 built-in       | 7 built-in    |
| **MCP**               | Client (eager)              | Client+Server            | Client             | Client          | Client          | None             | Client        |
| **MCP lazy loading**  | No                          | Yes                      | No                 | No              | No              | N/A              | No            |
| **Skills**            | SKILL.md (YAML+XML)         | SKILL.md + agents/       | SKILL.md           | No              | No              | No               | SKILL.md      |
| **Subagents**         | Framework (dormant)         | 3 built-in + custom      | Task types         | None            | 3 built-in      | None (by design) | None          |
| **Compaction**        | Mechanical pruning          | LLM summarization        | LLM + auto-retry   | Checkpointing   | Tiered + LLM    | Planned          | Session-based |
| **LLM summarization** | No                          | Yes                      | Yes                | No              | Yes             | No               | No            |
| **Sandboxing**        | App-level path check        | OS-level (84% reduction) | Sandbox + approval | Trusted folders | Per-agent       | None             | Config-based  |
| **OS sandbox**        | Planned (Landlock/Seatbelt) | seatbelt + network proxy | Container          | Yes             | No              | No               | No            |
| **Session persist**   | SQLite                      | Proprietary              | Session state      | None            | SQLite          | Files            | Files         |
| **Session resume**    | Yes                         | Yes                      | Yes                | No              | Yes             | Yes              | Yes           |
| **Session branching** | No                          | No                       | No                 | No              | No              | Yes              | No            |
| **Cost tracking**     | No                          | Yes                      | Yes                | Free tier       | Yes             | Yes              | No            |
| **LSP integration**   | No                          | No                       | No                 | No              | Yes             | No               | Yes           |
| **Web search**        | web_fetch (basic)           | No                       | No                 | Google Search   | No              | No               | No            |
| **Hooks**             | Framework (dormant)         | Config-driven            | No                 | No              | No              | No               | No            |
| **Instruction files** | AGENTS.md + CLAUDE.md       | CLAUDE.md                | codex.md           | GEMINI.md       | AGENTS.md       | pi.md            | crush.md      |
| **One-shot CLI**      | Designed (not impl)         | `claude -p`              | `codex run`        | `gemini -p`     | `opencode run`  | `pi --prompt`    | `crush run`   |
| **Config format**     | TOML (3-layer)              | JSON                     | TOML               | JSON            | JSON            | TOML             | JSON          |

---

## Prioritized Architectural Improvements

### P0 -- Blocking Competitive Parity

1. **LLM-based compaction (Tier 3)**: When Tier 1+2 pruning isn't enough, send the conversation to the LLM for summarization. Preserve: failed approaches, error messages, file paths, key decisions. Output a structured summary that replaces the conversation prefix. Wire `/compact` slash command. Without this, long sessions will hit context limits and lose critical context.

2. **Consume provider-reported usage**: The `StreamEvent::Usage` events are already emitted by providers but ignored by the agent loop. Wire them into `AgentEvent::TokenUsage` so compaction triggers use actual provider counts, not local estimates. This also unblocks cost tracking.

### P1 -- Core Functionality Gaps

3. **MCP lazy loading / tool search**: Stop including all MCP tool descriptions in the system prompt. Implement a `tool_search` meta-tool that describes available tools on demand. This is the difference between MCP being unusable (50+ tools = 72K wasted tokens) and practical.

4. **Wire the hook engine**: Implement config-driven hook registration from `[[hooks]]` TOML sections. Enable SessionStart and PreCompact hook points. Default hook: `cargo fmt` after write/edit to Rust files. This unlocks the extensibility story without new code.

5. **Register default subagents**: Ship Explorer (Haiku-class, read-only tools, fast model), Plan (read-only, inherits model), and General (all tools, inherits model) as built-in subagent configs. Register `spawn_subagent` in the default tool set. This matches Claude Code's subagent offering.

### P2 -- Quality and Polish

6. **Prompt caching for Anthropic**: Add `cache_control: ephemeral` breakpoint after the system prompt and AGENTS.md content. The architecture is already cache-friendly (stable system prompt prefix via RenderCache); the API-level hint is missing. This could reduce costs 50-90% on the cached prefix.

7. **Cost tracking**: With provider usage data flowing (item 2), implement per-session and per-turn cost calculation using `ModelPricing` data from the registry. Display in the status bar.

8. **Fix streaming assumptions**: Make `supports_tool_streaming()` configurable per-model or auto-detect via a test request, rather than hardcoding providers. Many OpenRouter models handle streaming+tools fine.

### P3 -- Differentiation Opportunities

9. **OS-level sandboxing**: Implement the designed Seatbelt (macOS) and Landlock (Linux) sandbox for bash child processes. This is the single biggest security improvement and follows Claude Code's proven approach (84% permission prompt reduction).

10. **LSP integration**: OpenCode and Crush both demonstrate the value of LSP for type-aware context. Even basic diagnostics (errors/warnings in changed files) would improve reliability. This is complex but high-impact for Rust/TypeScript codebases.

---

## Architectural Observations

**What ion gets right architecturally**: The decision to write native HTTP provider clients rather than depend on SDK crates. The binary Read/Write permission model rather than complex approval flows. Progressive skill loading. SQLite for sessions rather than filesystem. The move from ratatui to direct crossterm. These are all cases where the project chose the harder-to-build but correct long-term solution.

**Where ion is over-designed relative to current state**: The Plan-Act-Verify loop with the Designer subagent (plan generation for messages >100 chars) adds complexity without clear evidence of benefit. The `ToolMetadata`, `ToolSource`, and `ToolCapability` types exist but are disconnected from the Tool trait. The `RenderCache` for system prompt is correct architecture but may be premature optimization at current session lengths.

**Where ion is under-designed relative to peers**: Context management (no LLM summarization), MCP integration (no lazy loading), and cost tracking (no usage consumption) are the three areas where competitive agents are materially ahead. These are not UI polish issues -- they affect the core product experience.

**Key strategic question**: The memory system (OmenDB) was deferred as the key differentiator. None of the competitors have persistent semantic memory. But to get to the point where memory matters, the agent needs to handle long sessions without losing context -- which means LLM-based compaction is prerequisite infrastructure for the memory differentiator.
