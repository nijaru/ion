# ion Decisions

> Decision records for ion development. Historical Python-era decisions archived in DECISIONS-archive.md.

## 2026-01-23: Custom Text Entry with Ropey (Supersedes rat-text)

**Context**: rat-text proved problematic in practice - it didn't fit our use cases well and fighting the crate's assumptions was more work than building custom. Similar experience with other text input crates. We need full control over input handling for a terminal coding agent.

**Decision**: Build custom text entry using ropey as the text buffer backend.

| Requirement        | Implementation                                     |
| ------------------ | -------------------------------------------------- |
| Text buffer        | `ropey` crate (rope data structure)                |
| Grapheme handling  | `unicode-segmentation` for cursor/selection        |
| Multi-line editing | Custom with Shift+Enter for newlines               |
| OS movement keys   | Ctrl+Backspace, Option+Backspace, Cmd+arrows, etc. |
| Selection          | Custom selection state                             |
| Event handling     | Direct crossterm events                            |

**Rationale**: External text input crates (rat-text, tui-textarea, tui-input) were either over-engineered for our needs or missing critical features. Custom implementation with ropey gives us full control and avoids fighting upstream assumptions. Codex CLI takes this same approach.

**Also evaluating**: Whether ratatui adds value or if pure crossterm is simpler for our needs.

---

## 2026-01-20: Text Input Engine - rat-text (SUPERSEDED)

**Status**: SUPERSEDED by 2026-01-23 decision above.

**Context**: We need a long-term, multi-line editor with grapheme-safe cursor movement, selection, and consistent key handling. Custom input code risks edge cases and future refactors.

**Decision**: ~~Adopt `rat-text` as the unified text input engine for the main input and selector search.~~

| Requirement           | Coverage                    |
| --------------------- | --------------------------- |
| Multi-line editing    | `TextArea`                  |
| Selection             | built-in selection support  |
| Word/line navigation  | word/line helpers in API    |
| Undo/redo + clipboard | supported in `TextArea`     |
| Event handling        | `handle_events` (crossterm) |

**Rationale**: rat-text provides a full textarea with selection, navigation, and editing primitives. Using it avoids a bespoke editor and keeps future changes localized.

---

## 2026-01-20: Fuzzy Matching Library - fuzzy-matcher

**Context**: We need fuzzy search for provider/model selection now and for `@` file inclusion and slash commands later. We want low complexity and permissive licensing.

**Decision**: Use `fuzzy-matcher` for all fuzzy matching in the UI.

| Option        | Pros                          | Cons                  |
| ------------- | ----------------------------- | --------------------- |
| fuzzy-matcher | Simple, MIT, easy integration | Less advanced scoring |
| nucleo        | Very strong scoring/perf      | Heavier, MPL-2.0      |

**Rationale**: fuzzy-matcher is sufficient for current list sizes and features while keeping dependency surface small.

---

## 2026-01-20: License - PolyForm Shield

**Context**: The project is primarily a personal tool with optional future commercialization. We want permissive individual use while preventing competitors from building a competing product without a commercial agreement.

**Decision**: License the project under PolyForm Shield 1.0.0.

**Rationale**: Shield keeps the code public for individual and OSS use while reserving commercial competitive use for paid licensing later.

---

## 2026-01-19: Config Priority - Explicit Config > Env Vars

**Context**: Deciding whether API keys from config file or environment variables should take priority.

**Decision**: Config file takes priority over environment variables.

| Source      | Priority | Rationale                                   |
| ----------- | -------- | ------------------------------------------- |
| Config file | 1st      | Explicit user configuration for this tool   |
| Env var     | 2nd      | System-wide default, may not reflect intent |

**Rationale**: If a user explicitly puts a key in `~/.ion/config.toml`, that's an intentional choice. Environment variables are often set system-wide in shell configs and shared across many tools.

---

## 2026-01-19: Provider-Specific Model IDs

**Context**: Model IDs differ between OpenRouter (aggregator) and direct provider APIs.

**Decision**: Store model IDs as each provider expects them. No normalization.

| Provider           | Model ID Format   | Example                   |
| ------------------ | ----------------- | ------------------------- |
| OpenRouter         | `org/model`       | `anthropic/claude-3-opus` |
| Direct (Google)    | Native model name | `gemini-3-flash-preview`  |
| Direct (Anthropic) | Native model name | `claude-3-opus-20240229`  |

**Implications**:

- Switching providers requires re-selecting model (different APIs, different model names)
- Config stores `provider` and `model` as separate fields
- No prefix stripping needed for OpenRouter; strip for direct providers in client.rs

**Rationale**: OpenRouter IS a provider that happens to expose other providers' models. Its API expects `org/model` format. Trying to normalize creates complexity with little benefit.

---

## 2026-01-19: Unified Provider Enum (Completed)

**Context**: Found duplicate enum types that were nearly identical.

**Problem**: `Backend` (backend.rs) and `ApiProvider` (api_provider.rs) had:

- Same 6 variants
- Same `id()`, `name()`, `env_vars()` methods
- 1:1 mapping via `ApiProvider::to_backend()`

**Decision**: Unified into single `Provider` enum in `api_provider.rs`.

| Old Type    | New Type  | Changes                   |
| ----------- | --------- | ------------------------- |
| Backend     | (removed) | Deleted backend.rs        |
| ApiProvider | Provider  | Renamed, added `to_llm()` |

**Implementation**:

- Renamed `ApiProvider` to `Provider`
- Added `to_llm()` method for llm crate integration
- Removed `to_backend()` (no longer needed)
- Deleted `backend.rs`
- Updated all callers (client.rs, registry.rs, cli.rs, tui/mod.rs, model_picker.rs, provider_picker.rs)

---

## 2026-01-18: Rig Framework Evaluation (Removed)

**Context**: Evaluated **Rig** framework (rig.rs) for ecosystem compatibility. Built prototype `RigToolWrapper` bridge.

**Decision**: Remove Rig entirely. Not actively used; no concrete benefit over current architecture.

| Component  | Verdict | Rationale                         |
| ---------- | ------- | --------------------------------- |
| Agent Loop | Custom  | Plan-Act-Verify + OmenDB is key   |
| Providers  | Custom  | Working implementations exist     |
| Tools      | Custom  | MCP handles ecosystem interop     |
| MCP        | Custom  | `mcp-sdk-rs` integration complete |

**Outcome**: Removed `rig-core` dependency and `src/rig_compat/` module. If Rig ecosystem tools become relevant later, can add a thin adapter.

---

## 2026-01-18: ContextManager & minijinja Integration

**Context**: System prompt assembly was becoming monolithic and hard to maintain as we added Plans, Skills, and Memory context. Manual string concatenation broke provider caching.

**Decision**: Decouple assembly into a `ContextManager` using `minijinja` templates.

| Aspect         | Implementation                                                 |
| -------------- | -------------------------------------------------------------- |
| **Storage**    | `src/agent/context.rs`                                         |
| **Templating** | `minijinja` (Jinja2 compatible)                                |
| **Caching**    | Stabilized system prompt; injected memory as a `User` message. |

**Rationale**: Decoupling instructions from Rust code improves DX and allows for faster prompt iteration without recompiling.

---

## 2026-01-18: Plan-Act-Verify Loop

**Context**: Agents often suffer from "hallucination of success," assuming a tool call worked based on exit codes rather than actual output verification.

**Decision**: Implement a stateful task-tracking loop.

| Phase      | Implementation                                                                   |
| ---------- | -------------------------------------------------------------------------------- |
| **Plan**   | Designer sub-agent generates a JSON task graph with `TaskStatus`.                |
| **Act**    | Main agent focuses on the `Pending/InProgress` task.                             |
| **Verify** | ContextManager injects explicit verification instructions for every tool result. |

---

## 2026-01-16: Sub-Agents vs Skills Architecture

**Context**: Research on Pi-Mono (minimal) vs Claude Code (rich features), multi-agent effectiveness studies, and model routing patterns.

**Decision**: Sub-agents for context isolation only; skills for behavior modification.

### Skills (Same Context)

| Skill       | Purpose               |
| ----------- | --------------------- |
| `developer` | Code implementation   |
| `designer`  | Architecture planning |
| `refactor`  | Code restructuring    |

Skills inject prompts into the main context. Full conversation history preserved.

### Sub-Agents (Isolated Context)

| Sub-Agent    | Model | Purpose                     |
| ------------ | ----- | --------------------------- |
| `explorer`   | Fast  | Find files, search patterns |
| `researcher` | Full  | Web search, doc synthesis   |
| `reviewer`   | Full  | Build, test, analyze        |

Sub-agents spawn with isolated context, return summaries.

**Key insight** (Cognition research): Sub-agents making independent decisions produce incompatible outputs. "Actions carry implicit decisions." Skills preserve context; sub-agents isolate expansion.

### Model Selection

**Decision**: Binary choice (fast/full) for simplicity.

| Model | Use Case                               |
| ----- | -------------------------------------- |
| Fast  | Explorer only (Haiku-class, iterative) |
| Full  | Everything else (inherit from main)    |

**Rationale**: Complex routing adds failure modes. Explorer is iterative (5-10 searches), benefits from speed. Researcher/reviewer need reasoning quality.

**References**: `ai/design/sub-agents.md`, `ai/research/agent-comparison-2026.md`, `ai/research/model-routing-for-subagents.md`

---

## 2026-01-14: Rebrand to ion

**Context**: Need for a punchier, 4-letter CLI command that flows better and avoids the ambiguity of "Aircher."

**Decision**: Rename project to **ion**.

| Aspect    | Implementation                             |
| --------- | ------------------------------------------ |
| Binary    | `ion`                                      |
| CLI Style | 4-letter disemvoweled (classic Unix style) |
| Branding  | Neural/Agent identity                      |

## 2026-01-13: Architecture Refinements

**Context**: Research on RLM patterns, conversational subagents, and provider caching.

### No Hardcoded Models

**Decision**: All models/providers are user-configured, never hardcoded.

| Aspect    | Implementation                |
| --------- | ----------------------------- |
| Config    | TOML file, CLI args, env vars |
| Default   | User sets in config           |
| Switching | Runtime provider registry     |

### RLM is TUI Orchestration

**Decision**: RLM patterns are implemented in the TUI agent, not the model.

| RLM Component      | Who Implements           |
| ------------------ | ------------------------ |
| REPL environment   | TUI (context storage)    |
| Strategy selection | Model (via prompting)    |
| Sub-LM calls       | TUI (parallel API calls) |
| Result aggregation | Model (via prompting)    |

**Rationale**: The RLM paper's REPL is just an execution environment. The TUI implements the orchestration; any instruction-following model works.

### Conversational Subagents

**Decision**: Support multi-turn dialogue between main agent and subagents.

| Mode           | Use Case                 |
| -------------- | ------------------------ |
| SpawnAndWait   | Simple tasks (default)   |
| Conversational | Review loops, refinement |

**Research**: AutoGen Nested Chats, CAMEL role-playing patterns.

### Language: Rust (Confirmed)

**Decision**: Rust for the TUI agent.

| Factor         | Rust              | TypeScript/Bun            |
| -------------- | ----------------- | ------------------------- |
| Performance    | Excellent         | Good enough               |
| Distribution   | ~10MB single file | ~50MB+ with runtime       |
| Reference impl | Codex CLI (MIT)   | Claude Code (proprietary) |

**Why Rust works now**:

1. Codex CLI exists as MIT-licensed reference
2. ratatui has mature async patterns
3. OmenDB is Rust-native (no FFI)

### Configuration Architecture

**Decision**: Three-tier config with favorites support.

```
~/.config/ion/
├── config.toml          # Global settings
├── models.toml          # Model definitions + favorites
└── keys.toml            # API keys (separate for security)

./.ion/
├── config.toml          # Project overrides
```

---

## 2026-01-12: Full Rust with Native OmenDB

**Context**: After comprehensive research (Goose, Pi, memory architectures, TUI frameworks), deciding on final architecture.

**Decision**: Full Rust TUI agent with native OmenDB integration.

### Memory Integration

**Decision**: Use OmenDB Rust crate directly (`omendb = "0.0.23"`)

OmenDB is Rust-native with:

- HNSW + ACORN-1 filtered search
- Full-text search (tantivy)
- Same core as npm/Python packages

No reason to go through MCP when we can use the Rust crate directly.

### TUI Framework

**Decision**: ratatui

| Framework | Stars | Verdict                                        |
| --------- | ----- | ---------------------------------------------- |
| ratatui   | 10k+  | **Winner** - mature, async-friendly, good docs |

## 2026-01-14: Async TUI-Agent Communication

**Context**: Need to keep the TUI responsive (60fps) while the agent performs long-running tasks like LLM calls or tool execution.

**Decision**: Use `tokio::sync::mpsc` channels for communication.

| Component      | Responsibility                                                                                         |
| -------------- | ------------------------------------------------------------------------------------------------------ |
| **Agent Task** | Spawns in background; sends `AgentEvent` (deltas, tool starts, results) to channel.                    |
| **TUI App**    | Polls `event_rx` in every TUI update loop; updates local state (messages, running status) accordingly. |

**Rationale**: Avoids locking the UI thread and provides a clean separation of concerns. `mpsc` is the standard tool for this in the `tokio` ecosystem.

## 2026-01-14: Performance-First Architecture

**Context**: Initial benchmarks showed slow indexing (20ms/entry) and sequential tool execution bottlenecks.

**Decision**: Implement batching, parallel execution, and `Arc`-based history.

| Area         | Decision                                        | Rationale                                                                                                            |
| ------------ | ----------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| **Memory**   | Batch SQLite queries + Lazy OmenDB flush.       | Essential for real-time codebase indexing. Batching fixes the N+1 overhead of metadata retrieval.                    |
| **Agent**    | Parallel `execute_tools` + `Arc<Vec<Message>>`. | Allows fast multi-file reads. `Arc` prevents O(N^2) memory overhead in long sessions where N is the number of turns. |
| **Provider** | Use `Cow<'static, str>` for system prompts.     | Avoids unnecessary allocations for constant strings sent with every request.                                         |

**Rationale**: `ion` aims to be "Local-First", meaning local processing must be near-instant to compete with cloud-based agents.

## 2026-01-14: Agent Loop Decomposition

**Context**: The monolithic `run_task` loop is difficult to test and prone to hanging when sub-tasks (like provider streams) fail silently.

**Decision**: Decompose the loop into `stream_response` and `execute_turn_tools`.

**Rationale**:

1.  **Observability**: Easier to track exactly where a failure occurs (provider vs tool).
2.  **Safety**: Ensures error events are always propagated back to the TUI.
3.  **Testability**: Allows mocking tools without mocking the provider and vice versa.

## 2026-01-14: Post-Review Architecture Hardening

**Context**: Comprehensive review identified critical reliability and safety issues in the core Agent loop and Tool permission system.

**Decision**: Prioritize fix for "Lock-Across-Await" and "Silent Error Hiding".

| Component            | Decision                                                                                   | Rationale                                                                                                                       |
| -------------------- | ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------- |
| **Tool Permissions** | Drop `RwLock` read-guard _before_ calling `ask_approval`.                                  | Prevents deadlocks if the UI thread or a config update tries to acquire a write lock while the agent is waiting for user input. |
| **Agent Loop**       | Wrap provider stream in a robust error-handling block that always signals `tx` on failure. | Prevents the TUI from being stuck in "RUNNING" state when the backend stream crashes or errors out.                             |
| **Bash Tool**        | Use `kill_on_drop(true)` with user-cancellation support.                                   | Allows user to cancel long-running commands; process is killed on cancellation.                                                 |

**Rationale**: These are fundamental "correctness" and "safety" fixes that must precede Phase 3 feature completion (RAG/Compaction).

## 2026-01-15: Double-ESC Cancellation UX

**Context**: Need a way to cancel running commands/tasks without accidental triggers. Ctrl+C conflicts with text box clearing.

**Decision**: Double-press ESC within 1.5s window to cancel.

| Aspect   | Implementation                                            |
| -------- | --------------------------------------------------------- |
| Trigger  | ESC pressed twice within 1500ms                           |
| Feedback | Title bar turns yellow, shows "Press ESC again to cancel" |
| Scope    | Only active when agent is running                         |

**Rationale**: Single-key cancellation risks accidental triggers. Double-press is quick for intentional cancels but prevents accidents. ESC avoids Ctrl+C conflict with text clearing.

## 2026-01-17: Tree-sitter for Precise Symbol Mapping

**Context**: Regex-based symbol extraction is brittle for complex Rust (macros, nested modules) and TypeScript (nested classes, decorators).

**Decision**: Integrate `tree-sitter` for all codebase symbol extraction.

| Aspect          | Implementation                                                                       |
| --------------- | ------------------------------------------------------------------------------------ |
| **Parsing**     | Use `tree-sitter-rust`, `tree-sitter-typescript`, and `tree-sitter-python` grammars. |
| **Strategy**    | Query-based extraction for functions, structs, classes, and methods.                 |
| **Performance** | Tree-sitter is fast enough for lazy indexing during file interaction.                |

**Rationale**: Precise symbol mapping is critical for the `Explorer` sub-agent to navigate large codebases without getting lost in regex false positives.

## 2026-01-17: Strategist Sub-agent for Task Decomposition

**Context**: Complex user requests (>100 chars) often lead to "one-shot" failures where the agent tries to do too much at once or misses dependencies.

**Decision**: Implement a "Strategist" sub-agent that decomposes complex requests into a task graph.

| Aspect        | Implementation                                               |
| ------------- | ------------------------------------------------------------ |
| **Trigger**   | Automatically triggered for messages >100 characters.        |
| **Output**    | Structured JSON plan (task list with dependencies).          |
| **Execution** | The main agent follows the plan, updating status in the TUI. |

**Rationale**: Planning improves reliability for multi-step features. Decomposing the problem into smaller, verifiable chunks reduces the chance of catastrophic failure mid-task.

## 2026-01-17: Hybrid Model Discovery & Metadata

**Context**: Provider APIs (Anthropic, OpenAI) return model IDs but lack metadata like context window and pricing. OpenRouter is great but we want direct provider support without dependency.

**Decision**: Hybrid Discovery (Availability via Provider API + Metadata via models.dev).

| Phase         | Implementation                                 |
| ------------- | ---------------------------------------------- |
| **Discovery** | `GET /v1/models` from direct provider API.     |
| **Hydration** | Lookup ID in `models.dev` for pricing/context. |
| **Fallback**  | Use `models.dev` as primary for Anthropic.     |

**Rationale**: Separation of concerns. The provider tells us what is _available_ for the user's key; `models.dev` tells us _how_ to use it.

## 2026-01-17: Slash Commands & Discoverability

**Context**: Keybindings like `Ctrl+M` and `Ctrl+P` are efficient but not discoverable. Terminal users expect slash commands.

**Decision**: Support slash commands in the main chat input.

| Command      | Action                               |
| ------------ | ------------------------------------ |
| `/models`    | Open Model Picker (`Ctrl+M`)         |
| `/providers` | Open Provider Picker (`Ctrl+P`)      |
| `/clear`     | Reset session and wipe history       |
| `/snapshot`  | Trigger debug UI snapshot (`Ctrl+S`) |

**Rationale**: Increases discoverability and aligns with "Chat" mental model.

## 2026-01-17: Hybrid Embedding & ColBERT Strategy

**Context**: Evaluating embedding models for `hygrep` (Snowflake + MLX) and the upcoming OmenDB major database changes.

**Decision**: Implement a trait-based embedding engine and simulated multi-vector retrieval.

| Aspect           | Implementation                                                                         |
| ---------------- | -------------------------------------------------------------------------------------- |
| **Embedding**    | `EmbeddingProvider` trait to support local (Snowflake/ONNX) and cloud (OpenAI) models. |
| **Multi-Vector** | Simulated MaxSim retrieval (`Σ max(q_i · d_j)`) using document ID linking in OmenDB.   |
| **Scoring**      | Hybrid RRF (Vector + BM25) + ACE Counters + Time Decay.                                |

**Rationale**: Alignment with `hygrep` ensures consistency across the ecosystem. Simulated multi-vector support allows using SOTA ColBERT models before native OmenDB support is finalized.

## 2026-01-17: TUI Markdown Caching & Performance

**Context**: TUI rendering at 20-60 FPS was re-calculating markdown strings for every visible message, leading to excessive allocations and jitter.

**Decision**: Cache formatted markdown in `MessageEntry`.

| Aspect           | Implementation                                                             |
| ---------------- | -------------------------------------------------------------------------- |
| **Storage**      | `Option<String>` cache in `MessageEntry` struct.                           |
| **Invalidation** | Cache is updated only when new deltas are appended or during session load. |
| **Rendering**    | `draw` loop borrows `&str` directly from cache.                            |

**Rationale**: Fixes the hot-path allocation bug. Chat history is largely static once a turn is finished; re-parsing markdown on every frame is unnecessary.

## 2026-01-17: Optimized Local Batch Inference

**Context**: Sequential embedding of message batches (e.g. during session load) was underutilizing the CPU/GPU.

**Decision**: Implement manual tensor padding and true batching in ` SnowflakeArcticProvider`.

| Aspect         | Implementation                                                                      |
| -------------- | ----------------------------------------------------------------------------------- |
| **Batching**   | Use `Tokenizer::encode_batch` and construct a 2D `ndarray` with max-length padding. |
| **Pooling**    | Manual mean pooling using the attention mask to ignore padding tokens.              |
| **Validation** | Explicit check of model output dimension against provider config.                   |

**Rationale**: Significant performance boost for codebase indexing and session resumption.

---

## 2026-01-17: Strategist -> Designer Rename

**Context**: The term "Strategist" felt too abstract. "Designer" better reflects the role of architecting a solution before implementation.

**Decision**: Rename `Strategist` sub-agent to `Designer`.

---

## 2026-01-17: Context Caching Optimization (Stable System Prompt)

**Context**: Constantly changing the system prompt by injecting memory context breaks provider-side caching (Anthropic/DeepSeek).

**Decision**: Stabilize the `system_prompt` and inject memory context as a separate `User` message at the start of the current turn's history. This allows the system prompt to be cached and provides a stable prefix for conversation history.

---

## 2026-01-17: Production Hardening (Crate Selection)

**Context**: Preparing for Phase 6 (Production Scale).

**Decision**: Adopt the following libraries for core architecture:

- **minijinja**: For template-based prompt management (decoupling prompts from code).
- **thiserror**: For domain-specific, actionable error types in library modules.
- **figment**: For layered configuration (global + local + env).

## 2026-01-17: MCP Implementation Strategy (mcp-sdk-rs)

**Context**: Need for Model Context Protocol (MCP) client support for ecosystem compatibility. Evaluated multiple Rust SDKs (`mcp-sdk-rs`, `rust-mcp-sdk`, `rmcp`).

**Decision**: Use `mcp-sdk-rs` with manual `stdio` process bridging and `Session::Local`.

| Aspect                 | Choice                    | Rationale                                                           |
| ---------------------- | ------------------------- | ------------------------------------------------------------------- |
| **Crate**              | `mcp-sdk-rs` (v0.3.4)     | Mature JSON-RPC implementation, aligns with project references.     |
| **Transport**          | `Stdio`                   | Primary transport for local coding tools (Claude Desktop standard). |
| **Process Management** | `tokio::process::Command` | Full control over stdin/stdout piping before handoff to transport.  |
| **Configuration**      | `.mcp.json` support       | Compatibility with existing tool ecosystems (Cursor, Claude Code).  |

**Rationale**: `mcp-sdk-rs` provides a solid session-based model. By using `Session::Local`, we offload the complexity of process management to a proven pattern while maintaining a clean `Tool` trait abstraction for the Agent.

**Impact**: Enables the agent to use thousands of external tools with minimal context overhead (lazy tool loading).

---

## 2026-01-18: TUI Design Direction (Claude Code Style)

**Context**: Initial TUI used Nerd Font icons and heavy visual elements. User feedback preferred minimal, clean design like Claude Code / pi-mono.

**Decision**: Adopt Claude Code's minimal aesthetic.

| Element       | Before                 | After                                   |
| ------------- | ---------------------- | --------------------------------------- |
| Chat headers  | Nerd Font icons        | User `>` prefix; no agent header        |
| Status line   | Verbose keybindings    | `model · context%` left, `? help` right |
| Loading       | "Agent is thinking..." | "Ionizing..."                           |
| Provider list | Name + description     | Name + auth hint only                   |

**Rationale**: Minimal UI reduces cognitive load. Power users discover features via help modal. Aligns with terminal tool conventions.

---

## 2026-01-18: Git Integration & Undo Strategy

**Context**: Evaluated the need for automated git commits and a hidden "undo" checkpoint system for agent-driven file edits.

**Decision**: Rejection of automated background commits. Support for manual session checkpointing only.

| Component          | Decision   | Rationale                                                                                         |
| :----------------- | :--------- | :------------------------------------------------------------------------------------------------ |
| **Git Automation** | **No**     | Avoids polluting user history with "noisy" agent commits. Aligns with mandates.                   |
| **Undo/Rollback**  | **Manual** | User-managed via standard git tools (`git checkout`, `git reset`). Simple and reliable.           |
| **Checkpointing**  | **Manual** | Checkpoint session state to `ai/` and `.tasks/` only when explicitly requested or at session end. |

**Rationale**: The complexity of getting an "undo" system right across all OS platforms outweighs the benefit when the user already has professional version control tools.

---

## 2026-01-18: Rendered Context Caching (tk-tly3)

**Context**: Context assembly rendered the entire system prompt template on every turn, even if the plan and skill remained identical.

**Decision**: Implement a `RenderCache` in `ContextManager`.

| Aspect           | Implementation                                                             |
| :--------------- | :------------------------------------------------------------------------- |
| **Trigger**      | Comparison of `Plan` (PartialEq) and `Skill` (PartialEq).                  |
| **Storage**      | `Mutex<Option<RenderCache>>` within `ContextManager`.                      |
| **Invalidation** | Cache is updated only when the active plan, task status, or skill changes. |

**Outcome**: Reduced local CPU overhead and ensured bit-for-bit stability of the system prompt prefix for provider-side prompt caching.

---

## 2026-01-18: Slash Command System

**Context**: Need autocomplete for slash commands with fuzzy matching, similar to Claude Code. Also want commands triggerable mid-prompt.

**Decision**: Fuzzy slash command autocomplete with inline trigger support.

| Feature           | Implementation                                  |
| ----------------- | ----------------------------------------------- |
| **Autocomplete**  | Fuzzy match on `/` prefix, dropdown above input |
| **Mid-Prompt**    | Detect `/command` anywhere, extract and execute |
| **Extensibility** | Commands registered via trait, easy to add new  |

**Commands (Initial)**:

- `/model` - Model picker
- `/provider` - Provider picker
- `/clear` - Clear conversation
- `/index [path]` - Index codebase
- `/quit` - Exit
- `/help` - Help modal
- `/settings` - Settings (future)
- `/compact` - Trigger compaction (future)
- `/resume` - Resume session (future)

---

## 2026-01-18: Session Retention Policy

**Context**: Need to decide how long to keep conversation history.

**Decision**: 30-day default retention, configurable.

| Setting          | Default | Range   |
| ---------------- | ------- | ------- |
| `retention_days` | `30`    | `1-365` |

**Implementation**: Background cleanup on startup, delete sessions where `updated_at < now - retention_days`.

---

## 2026-01-18: First-Time Setup Flow

**Context**: No default provider/model - users must explicitly choose.

**Decision**: On first launch (no config), force provider → model selection before chat.

| State           | Behavior                                   |
| --------------- | ------------------------------------------ |
| No provider set | Open provider picker, block until selected |
| No model set    | Open model picker, block until selected    |
| Config exists   | Load saved provider/model                  |

**Rationale**: Avoids assumptions about user's API keys and preferred models. Explicit is better than implicit.

---

## 2026-01-18: Memory System Deferral

**Context**: Memory (OmenDB) is the key differentiator, but TUI agent needs to be fully functional first.

**Decision**: Defer memory integration until core TUI is stable.

**Priority Order**:

1. TUI agent fully working (current focus)
2. Session management (continue/resume)
3. Context tracking & cost display
4. Memory integration (Phase 6)

**Rationale**: Ship a working agent first, then add the differentiating features. Memory adds complexity (embeddings, indexing, retrieval) that shouldn't block MVP.

---

## 2026-01-18: Plugin Architecture (Claude Code Compatible)

**Context**: Need plugin system for memory integration. Evaluated Claude Code, OpenCode, and pi-mono plugin ecosystems.

**Decision**: Implement Claude Code-compatible hook system.

| Aspect            | Decision                         | Rationale          |
| ----------------- | -------------------------------- | ------------------ |
| **Format**        | Claude Code compatible           | Largest ecosystem  |
| **Hook Types**    | `command` only (shell/binary)    | Simplest, portable |
| **Memory Plugin** | Separate crate, loaded via hooks | Clean separation   |
| **MCP Support**   | Keep existing                    | Already working    |

**Hook Events** (matching Claude Code):

- `SessionStart`, `SessionEnd`
- `UserPromptSubmit` (memory injection)
- `PreToolUse`, `PostToolUse`, `PostToolUseFailure`
- `PreCompact` (memory save)
- `Stop`, `Notification`

**Memory Plugin Compatibility**: OmenDB memory plugin requires UserPromptSubmit, PostToolUse, SessionStart, PreCompact - all supported.

**Design Doc**: `ai/design/plugin-architecture.md`

---

## 2026-01-18: Thinking Mode Toggle

**Context**: Need thinking mode toggle for reasoning models.

**Decision**: Ctrl+T cycles through levels, display in input box.

| Setting                 | Value                                                       |
| ----------------------- | ----------------------------------------------------------- |
| **Keybinding**          | Ctrl+T                                                      |
| **Levels**              | off → low → med → high → off                                |
| **Display**             | `[low]` / `[med]` / `[high]` in input box title, right side |
| **When off**            | No display                                                  |
| **Persistence**         | Global, persists across models                              |
| **Non-thinking models** | Auto-disable, no indicator                                  |

**Display Format**:

```
┌ [WRITE] ─────────────────────────── [med] ┐
│ > input                                    │
└────────────────────────────────────────────┘
```

---

## 2026-01-18: Mode Toggle Keybinding

**Context**: Tab for mode toggle is too easy to hit accidentally.

**Decision**: Change mode toggle from Tab to Shift+Tab.

| Before | After     |
| ------ | --------- |
| Tab    | Shift+Tab |

**Rationale**: Shift+Tab is harder to hit accidentally, matches Claude Code convention.

---

## 2026-01-18: Help Modal Key Display

**Context**: Deciding between "^" and "Ctrl" notation.

**Decision**: Use "Ctrl" for readability.

| Before | After  |
| ------ | ------ |
| ^M     | Ctrl+M |
| ^P     | Ctrl+P |
| ^C     | Ctrl+C |
| ^T     | Ctrl+T |

**Rationale**: "Ctrl" is more readable for newcomers. Worth the extra characters.

---

## 2026-01-18: Model Picker Improvements

**Context**: Model picker showing $0.00 for all prices, unsorted list, unclear column headers.

**Decision**: Comprehensive model picker overhaul.

| Aspect          | Implementation                                                                 |
| --------------- | ------------------------------------------------------------------------------ |
| **Pricing**     | Parse string prices from OpenRouter API (e.g., "0.000003" per token → $3.00/M) |
| **Filtering**   | Filter models with negative pricing (-1 = variable/routing models)             |
| **Sorting**     | Sort by org first, then newest (using `created` timestamp from API)            |
| **Headers**     | Model \| Org \| Context \| Input \| Output                                     |
| **Free models** | Show "free" in green instead of "$0.00"                                        |
| **Tab nav**     | Tab switches between provider and model pickers                                |

**Rationale**: Users need accurate pricing info and logical grouping to find models quickly.

---

## 2026-01-18: CLI One-Shot Mode Design (Revised)

**Context**: Need non-interactive mode for scripting, testing, and automation. Researched Claude Code, Gemini CLI, Codex, aider, goose patterns.

**Decision**: Subcommand style only (`ion run`). Dropped dual `-p` flag to reduce complexity.

| Pattern | Syntax                               | Notes                                  |
| ------- | ------------------------------------ | -------------------------------------- |
| Basic   | `ion run "prompt"`                   | Explicit, matches Codex/goose/OpenCode |
| Stdin   | `ion run -`                          | Prompt from stdin                      |
| Context | `cat file \| ion run "analyze"`      | Piped content as context               |
| File    | `ion run -f context.txt "prompt"`    | File as context                        |
| Model   | `ion run -m provider/model "prompt"` | Flags after subcommand                 |
| Output  | `ion run -o json "prompt"`           | Short: `-o`, long: `--output-format`   |

**Essential flags**:

| Flag                     | Purpose                                 |
| ------------------------ | --------------------------------------- |
| `-m` / `--model`         | Model selection (provider/model format) |
| `-o` / `--output-format` | `text` (default), `json`, `stream-json` |
| `-q` / `--quiet`         | Response only, no progress              |
| `-y` / `--yes`           | Auto-approve all tool calls             |
| `--max-turns N`          | Limit agentic turns (prevent runaway)   |
| `-c` / `--continue`      | Continue last session                   |
| `--no-session`           | Don't persist session                   |
| `-v` / `--verbose`       | Detailed output                         |
| `--no-tools`             | Disable tools (pure chat)               |
| `-f` / `--file`          | Include file as context                 |
| `--cwd`                  | Working directory                       |

**Exit codes**:

| Code | Meaning                   |
| ---- | ------------------------- |
| 0    | Success                   |
| 1    | Error (API, tool failure) |
| 2    | Interrupted (Ctrl+C)      |
| 3    | Max turns reached         |

**Why subcommand only**: Dual entry points (`run` + `-p`) add complexity for minimal benefit. Since ion uses OpenRouter's provider/model format (like OpenCode), subcommand style is more consistent.

**Research**: See `ai/research/cli-oneshot-patterns-2026.md`

---

## 2026-01-18: Remove Memory System, Switch to Stable Rust

**Context**: OmenDB requires nightly Rust for `portable_simd`. Memory system is the key differentiator but TUI agent should be fully functional first.

**Decision**: Archive memory code, switch to stable Rust.

| Action                | Implementation                    |
| --------------------- | --------------------------------- |
| **Archive**           | Pushed to nijaru/ion-archive repo |
| **Removal**           | Deleted src/memory/, all refs     |
| **Toolchain**         | rust-toolchain.toml → stable      |
| **Re-implementation** | After TUI agent is fully working  |

**Rationale**: Ship a working, stable agent first. Memory adds complexity that shouldn't block core functionality. Stable Rust is more accessible for contributors

---

## 2026-01-19: Migrate to llm Crate for Provider Support

**Context**: Custom provider implementations (anthropic.rs, openai.rs, openrouter.rs, ollama.rs) totaled 2,500+ lines with duplicated streaming, error handling, and tool calling logic. User feedback: "We should probably be using tested libs as much as possible to reduce our maintenance and testing required."

**Decision**: Replace custom providers with `llm` crate.

| Aspect           | Implementation                                                |
| ---------------- | ------------------------------------------------------------- |
| **Crate**        | `llm = "1.3"` with selective features                         |
| **Features**     | `openai`, `anthropic`, `ollama`, `groq`, `google`             |
| **Architecture** | `Backend` enum + `Client` struct + `LlmApi` trait             |
| **Providers**    | OpenRouter, Anthropic, OpenAI, Ollama, Groq, Google (6 total) |

**Current Architecture** (after tk-gpdy unification):

| File              | Purpose                                   |
| ----------------- | ----------------------------------------- |
| `api_provider.rs` | Unified `Provider` enum with all metadata |
| `client.rs`       | Client implementing LlmApi via llm crate  |
| `error.rs`        | Clean error types                         |
| `types.rs`        | Shared types (StreamEvent, Message, etc.) |
| `registry.rs`     | Model discovery and caching               |

**Removed**: anthropic.rs, openai.rs, openrouter.rs, ollama.rs, llm_provider.rs, backend.rs

**Rationale**: Battle-tested library with streaming, tool calling, and multi-provider support. Reduces maintenance burden significantly. Only 10 small dependencies added with selective features.

## 2026-01-19: Streaming+Tools is Core Requirement, Not Edge Case

**Context**: Google provider fails with "streaming with tools not supported" when using llm crate. Added fallback to non-streaming, but this is degraded UX (no incremental text output). User pointed out: tools are ALWAYS present for a coding agent, so streaming+tools is the primary use case.

**Decision**: Research and potentially build custom modular streaming interface.

**Options Considered**:

1. Keep llm crate with non-streaming fallback - degraded UX for some providers
2. Find alternative crate that supports streaming+tools for all providers
3. Build custom provider layer with unified StreamEvent interface
4. Contribute streaming+tools fix to llm crate upstream

**Next Steps** (tk-g1fy, tk-e1ji):

- Research each provider's streaming API format (OpenAI SSE, Anthropic SSE, Google, Ollama)
- Design unified `StreamEvent` enum that normalizes all formats
- Evaluate build vs buy decision based on research

**Rationale**: All major terminal agents (Claude Code, Pi, Codex) stream text. Non-streaming feels frozen. For good UX across all providers, we need streaming+tools to work everywhere.

---

## 2026-01-27: TUI v2 - Drop ratatui, Use crossterm Directly

**Context**: `Viewport::Inline(15)` creates a fixed 15-line viewport at the bottom of the terminal. Our UI needs dynamic height (input box grows/shrinks with content). This mismatch causes gaps and visual bugs. Research showed Codex CLI doesn't use `Viewport::Inline` either - they use custom terminal management.

**Decision**: Remove ratatui entirely, use crossterm for direct terminal I/O.

| Component    | Before (ratatui)                | After (crossterm)                   |
| ------------ | ------------------------------- | ----------------------------------- |
| Chat history | `insert_before()` to scrollback | `println!()` to stdout              |
| Bottom UI    | Fixed `Viewport::Inline(15)`    | Cursor positioning, dynamic height  |
| Widgets      | Paragraph, Block, etc.          | Direct ANSI/box-drawing characters  |
| Diffing      | Automatic cell diffing          | TBD (may not need with sync output) |

**Architecture**:

```
Native scrollback (stdout)     Managed bottom area (crossterm)
├── Header (ion, version)      ├── Progress (1 line)
├── Chat history               ├── Input (dynamic height)
├── Tool output                └── Status (1 line)
└── Blank line after each
```

**What we keep**: Word wrap algorithm, cursor positioning logic, syntax highlighting - all already implemented in composer/mod.rs and highlight.rs.

**Open questions** (need research):

1. Is cell/line diffing needed, or is synchronized output (CSI 2026) enough?
2. How to handle terminal resize cleanly?
3. How to render streaming responses before complete?
4. How to handle modal UI (selectors) without Viewport?
5. Should we replace llm-connector for model quirks (Kimi reasoning field)?

**Rationale**: ratatui's Viewport::Inline doesn't support dynamic height. Fighting the framework is worse than not using it. Our actual needs (styled text, borders, cursor positioning) are simple enough that crossterm alone suffices.

**Design doc**: `ai/design/tui-v2.md`

---

## 2026-01-31: Code Refactor Sprint - File Splits & Architecture Prep

**Context**: Codebase audit identified 7 files >700 lines, performance hot paths, and architecture needs for extensibility.

**Decision**: Four-phase refactor sprint with incremental commits.

| Phase | Goal                 | Outcome                                               |
| ----- | -------------------- | ----------------------------------------------------- |
| 1     | Performance + idioms | CTE query, single-pass take_tail, format!→Print       |
| 2     | File splits          | composer (1103→4), highlight (841→5), session (740→6) |
| 3     | Code deduplication   | PickerNavigation trait (18→6 match arms)              |
| 4     | Architecture prep    | Hook system, tool metadata types                      |

**Hook System** (`src/hook/mod.rs`):

- `HookPoint`: PreToolUse, PostToolUse, OnError, OnResponse
- `HookContext`: Carries data to hooks
- `HookResult`: Continue, Skip, ReplaceInput/Output, Abort
- `HookRegistry`: Priority-ordered execution

**Tool Metadata** (`src/tool/types.rs`):

- `ToolSource`: Builtin, Mcp { server }, Plugin { path }
- `ToolCapability`: Read, Write, Execute, Network, System
- `ToolMetadata`: name, description, source, capabilities, enabled

**Deferred** (lower priority):

- File splits: openai_compat/client.rs, registry.rs, events.rs, render.rs
- Completer logic deduplication
- Dynamic tool loading integration

**Rationale**: Smaller files improve navigation and cognitive load. Hook system enables future extensibility (logging, rate limiting, content filtering) without core changes.
