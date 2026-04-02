# ion Decisions

Append-only history of architectural and design decisions for `ion`.

---

## 2026-04-02 — Runtime: Retry transient provider failures silently before surfacing the final error

**Context:** Native LLM providers can fail transiently with rate limits or short-lived transport issues. The inline TUI should stay clean and avoid duplicating the same failure in both the transcript and the progress surface.

**Decision:** Wrap native providers in canto's retry layer so transient generation and streaming failures retry automatically with exponential backoff. Only the final failure is surfaced to ion if all attempts are exhausted.

**Rationale:**

1. **Better default behavior:** Most transient failures clear on their own and should not require user intervention.
2. **Cleaner UI:** Silent retries avoid noisy duplicate error presentation in Plane B and scrollback.
3. **Simple boundary:** Ion gets a stable provider surface; canto owns the retry mechanics.

**Tradeoffs:** Retries add a small delay before final failure and can make repeated transient provider issues less visible unless they exhaust the retry budget.

---

## 2026-04-02 — Commands: built-in actions use `/`, user-defined aliases use `//`

**Context:** ion needs a textual control plane for explicit runtime actions, and the user wants a clear place for custom slash commands and skills. Hotkeys are a poor fit for everything, especially as the UI grows and more actions become stateful or discoverable-by-name rather than discoverable-by-key.

**Decision:** Use `/foo` for built-in ion commands and user-facing runtime actions. Reserve `//foo` for user-defined command or skill aliases. Keep hotkeys sparse and use them mainly to open modal surfaces rather than to encode every action directly.

**Rationale:** This keeps the command model explicit and searchable, avoids hotkey sprawl, and leaves room for user extension without colliding with built-in commands. It also aligns with the broader terminal-agent pattern where typed commands are the primary control plane and hotkeys are accelerators.

---

## 2026-04-02 — Models: expose `primary` and `fast`, keep `summary` internal

**Context:** ion needs a simple model-preset model that works for daily use, subagents, and cheap transforms like summarization without exposing every provider-specific raw model in the TUI.

**Decision:** Use `primary` and `fast` as the only UI-visible model presets. Keep `summary` config-only for compaction, titles, and other cheap transforms. Make `Ctrl+P` the quick toggle between `primary` and `fast`, and keep full model/provider selection behind fuzzy slash commands.

**Rationale:** `primary` / `fast` is clearer than `primary` / `deep`, avoids confusion with thinking budget, and gives the agent a stable preset vocabulary. `summary` is an implementation concern, not a daily workflow preset. The quick toggle stays ergonomic while the full selection surface remains discoverable and text-driven.

**Tradeoffs:** This reduces the number of UI-exposed choices, but it simplifies the mental model and leaves room for provider-specific defaults to be resolved deterministically from the model catalog.

---

## 2026-04-02 — Commands: add explicit `/primary` and `/fast` preset switches

**Context:** The model preset model now has a runtime toggle (`Ctrl+P`) and a textual control plane. Users still need explicit, discoverable commands for switching between the active model presets without relying on a keybinding.

**Decision:** Add `/primary` and `/fast` as built-in commands that switch the active preset slot. `/model` and `/thinking` continue to mutate whichever preset is active, while the fast slot is persisted through `fast_model` and `fast_reasoning_effort` in config.

**Rationale:**

1. **Discoverability:** Typed commands are easier to remember and script than keyboard state.
2. **Low ceremony:** `Ctrl+P` remains the quick toggle, while the slash commands provide an explicit control path.
3. **Clear persistence model:** The primary slot stays anchored by `provider` / `model` / `reasoning_effort`; the fast slot has dedicated fields.

**Tradeoffs:** This adds a little more command surface, but it keeps the model preset system understandable and avoids overloading a single hotkey.

---

## 2026-04-02 — Hotkeys: use `Ctrl+P` for primary/fast and `/model` for explicit selection

**Context:** The primary user action is swapping between `primary` and `fast`, not hopping into a separate explicit model picker. A direct model hotkey adds another global chord without enough daily value to justify it.

**Decision:** Keep `Ctrl+P` as the primary/fast swap. Do not reserve a separate model hotkey. `/model` remains the explicit model-selection surface, and the picker work should focus on provider/model/favorites scopes invoked from the textual command path or from the picker flow itself.

**Rationale:**

1. **Common case wins:** primary/fast swapping is the highest-frequency model action.
2. **Smaller keymap:** removing the dedicated model key frees a global chord for no loss of core functionality.
3. **Textual explicitness:** `/model` is still the explicit, discoverable path for full model selection.

**Tradeoffs:** Explicit model selection takes a text command or picker invocation instead of a hotkey, but that is acceptable because it is not the primary daily operation.

---

## 2026-04-02 — Versioning: v0.0.0 makes no compatibility promises

**Context:** ion is still unstable dev. Preserving old bindings, config shapes, or implementation quirks because they already exist would freeze the wrong choices into the product.

**Decision:** Treat the current surface as disposable. Choose the final shape for each workflow directly, and replace any binding, preset, or config choice when a better one is identified. Do not add backward-compatibility shims, temporary bindings, or migration paths unless they are explicitly required.

**Rationale:**

1. **Final-shape design:** The v0.0.0 contract is to build the right interface, not to preserve earlier experiments.
2. **No lock-in:** Early choices should not become architecture by accident.
3. **Cleaner implementation:** Removing compatibility debt keeps the code and docs aligned with the current product decision.

**Tradeoffs:** Users may need to adjust to changed bindings or config names while the UI is still evolving, but that is acceptable in an unstable release.

---

## 2026-04-01 — Planning: Use Pi as a maturity benchmark, not a hard parity gate

**Context:** Recent planning around Pi, Claude Code, subagents, and swarm mode risked sounding like ion should chase literal feature parity before moving forward. At the same time, the team explicitly wants advanced orchestration work to wait until the single-agent inline path feels stable and feature-complete.

**Decision:** Treat Pi as a rough benchmark for maturity, not as a hard gate. The actual prerequisite for subagents and swarm-oriented features is a trustworthy inline single-agent loop and TUI in ion itself.

**Rationale:**

1. **Right gate:** Stability and clarity in ion's own core loop matter more than matching another tool's feature list.
2. **Better sequencing:** This keeps work ordered as inline stability first, then subagent runtime, then inline subagent UI, then swarm mode later.
3. **Less cargo-culting:** Benchmarking against Pi is useful for taste and completeness, but it should not force timing or architecture decisions.

**Tradeoffs:** Some Pi-adjacent features may land later than they would in a literal parity chase, but the base product stays cleaner and more reliable.

---

## 2026-04-01 — TUI: Keep inline chat primary and reserve alternate-screen for swarm orchestration

**Context:** ion needs to support subagents and eventually a richer orchestration view, but the current product is still centered on direct inline chat. Competing TUIs often push subagent activity directly into transcript history, which adds noise and makes the conversation harder to scan.

**Decision:** Keep inline mode as the primary chat experience. Render active subagent activity as ephemeral Plane B state, and reserve a future alternate-screen view for explicit swarm or operator workflows rather than general chat.

**Rationale:**

1. **Inline remains the right default:** Most ion use is still direct user-to-agent interaction.
2. **Transcript should stay durable and readable:** Start/completion/failure events can be committed, while live child deltas should stay ephemeral.
3. **Swarm supervision wants a different layout:** Once users are managing multiple workers, tasks, retries, and handoffs, the product shifts from chat to orchestration and benefits from a dedicated view.

**Tradeoffs:** Some detailed child execution information will be less visible in scrollback by default, but the transcript stays cleaner and the future dashboard has a clearer role.

---

## 2026-04-01 — Architecture: Treat Pi and Claude Code as product references, not implementation templates

**Context:** Pi and Claude Code both contain strong ideas for coding-agent UX and runtime behavior, but both come from architectures that are not native to ion's Go + Bubble Tea v2 codebase. Earlier planning notes risked reading like a backlog of literal imports.

**Decision:** Use Pi and Claude Code as reference material for product behavior and framework boundaries, but only adopt patterns that map cleanly to ion's existing `Model`/`Msg`/`Cmd` architecture or to reusable primitives in `canto`.

**Rationale:**

1. **Language fit:** Go and Bubble Tea want explicit state, message-driven transitions, and small concrete types rather than JSX, reconcilers, or generalized component frameworks.
2. **Boundary clarity:** Reusable runtime ideas belong in `canto`; terminal UX behavior belongs in `ion`.
3. **Complexity control:** New layers should be justified by a concrete ion problem, not by symmetry with another tool's internals.

**Tradeoffs:** Some ideas that look attractive in competitor systems will stay deferred or rejected until ion hits a real need that warrants them.

---

## 2026-03-17 — Framework: Adopt canto as the core engine

**Context:** The initial `ion` rewrite used a direct, Gemini-specific backend. This was difficult to maintain and lacked support for tool output streaming, multi-agent coordination, and robust session persistence.

**Decision:** Refactor `ion` to use the `canto` framework as its primary agent runtime.

**Rationale:**

1. **Separation of Concerns:** `canto` handles the LLM lifecycle, tool execution, and session logging, while `ion` focuses on the TUI and coding-specific UX.
2. **Persistence:** `canto` provides a robust SQLite event store with FTS5 search.
3. **Observability:** `canto`'s event-driven architecture maps perfectly to the reactive `bubbletea` UI.
4. **Future-Proofing:** `canto` already supports multi-agent graphs, memory systems, and MCP, which `ion` can now leverage.

**Tradeoffs:** Introduces a dependency on a separate (though local) framework.

---

## 2026-03-17 — Storage: Unified SQLite event store via CantoStore

**Context:** `ion`'s original storage model used a mix of JSONL files and a separate SQLite index. This led to data fragmentation and increased complexity.

**Decision:** Migrate all session and input storage to a single SQLite database powered by `canto.session.SQLiteStore`.

**Rationale:**

1. **Simplicity:** One file (`~/.ion/ion.db`) stores everything.
2. **Durability:** Leverages SQLite's WAL mode for safe concurrent access.
3. **Search:** Enables built-text search across all session history.
4. **Consistency:** Using the same store for the UI and the backend ensures the transcript always matches the backend state.

**Tradeoffs:** No longer human-readable via `cat` (requires `sqlite3`), but FTS5 makes it more searchable than JSONL.

---

## 2026-03-17 — Memory: Implicit Project Awareness via Background Indexing

**Context:** The initial memory system relied on explicit `recall` and `memorize` tools. This requires the agent to "know what it doesn't know" and manually save insights, which is a high cognitive load and often results in missing codebase context.

**Decision:** Pivot to an **Implicit Memory** model. Build an autonomous background scanner that chunks and indexes the project into the `knowledge` table during idle time.

**Rationale:**

1. **Zero-effort Context:** The agent gains project-level awareness without the user or the agent having to manually curate it.
2. **Searchability:** Multi-stage indexing (File Summaries, Segment Chunks, Insights) provides multiple granularties of search.
3. **Cross-session Continuity:** Changes in the codebase are automatically tracked and reflected in subsequent sessions via the indexed metadata.

**Tradeoffs:**

- Increased complexity in `internal/storage` for indexing logic.
- Background overhead (mitigated by using idle time and file hashing).
- Search quality depends on chunking/summarization strategies.

---

## 2026-03-23 — Context: Pivot to Explicit @file Tags & Approvals

**Context:** The implicit background memory system (indexed via background scanner) proved too heavy and often provided irrelevant or overwhelming context. Users needed more direct control over what the agent sees.

**Decision:** Prioritize explicit `@file` tags in the composer and implement an interactive `ApprovingTool` for sensitive operations.

**Rationale:**

1. **User Control:** `@file` allows users to surgically provide the exact context needed for a task, reducing token waste and model confusion.
2. **Framework Alignment:** Using `canto.context.RequestProcessor` for `@file` resolution (via `FileTagProcessor`) keeps the backend clean and leverages the framework's phased context pipeline.
3. **Safety:** Wrapping the `bash` tool with an `ApprovalManager` ensures that destructive actions (like `rm` or `git push`) require a manual `[y/N]` confirmation in the TUI, matching the security standards of established agents.
4. **Efficiency:** Pausing the background indexer reduces CPU/IO jitter and allows for a more focused development experience.

**Tradeoffs:**

- Requires manual effort from the user to tag files.
- Approvals add a turn-interrupting "speed bump" for the agent.

---

## 2026-03-23 — Editing: Multi-Patch Atomic Operations

**Context:** The initial `edit` tool was a simple string-replace that failed on ambiguity and lacked atomicity when changing multiple files or multiple locations in one file.

**Decision:** Implement a `multi_edit` tool that performs a two-pass atomic update (Validate all -> Write all).

**Rationale:**

1. **Atomicity:** Ensures that either all changes are applied or none are, preventing partial file corruption.
2. **Robustness:** Validates all `old_string` matches before writing any changes.
3. **Efficiency:** Reduces the number of tool turns needed for cross-file refactoring.

**Tradeoffs:** Requires in-memory buffering of multiple files during the validation phase.

---

## 2026-03-23 — Protocol: Integrated MCP Client Surface

**Context:** The Model Context Protocol (MCP) has emerged as the standard for connecting agents to external tools and data sources. Ion needed a way to leverage this ecosystem.

**Decision:** Integrate the official MCP Go SDK into the `CantoBackend` and expose a `/mcp add` command in the TUI.

**Rationale:**

1. **Ecosystem Compatibility:** Allows Ion to use any MCP-compliant server (e.g. Brave Search, GitHub, Google Drive).
2. **Dynamic Discovery:** Tools are discovered and registered at runtime, allowing users to extend the agent's capabilities without restarting the session.
3. **Framework Alignment:** Leveraging Canto's built-in MCP client simplifies the implementation and ensures robust transport handling.

**Tradeoffs:**

- Adds a dependency on external MCP server processes.
- Requires manual configuration via slash-commands.

---

## 2026-03-23 — Verification: High-Fidelity Auto-Verification Loop

**Context:** AI edits often introduce subtle regressions or syntax errors that are only caught during a subsequent manual test run.

**Decision:** Implement a `verify` tool and update the system prompt to mandate an explicit verification turn after every edit.

**Rationale:**

1. **Confidence:** Ensures that every change is behavioral and structurally sound before the agent proceeds.
2. **Observability:** Structured `VerificationResult` events in the TUI provide clear PASSED/FAILED signals to the user.
3. **RLM Foundation:** Establishes the "Reinforcement Learning from Metadata" loop where agent performance is judged by objective verification outcomes.

**Tradeoffs:**

- Increases the number of tokens and turns per task.
- Requires well-defined test suites in the target project.

---

## 2026-03-23 — TUI: Adoption of Plane A/B Inline Rendering

**Context:** The boxed viewport approach (`Viewport::Inline`) used in Phase 1 and 2 led to poor native terminal ergonomics. Users could not easily select text above the viewport, and the fixed height caused "context blindness" and UI flicker.

**Decision:** Refactor the TUI to use a Two-Plane Inline model. Plane A (History) is flushed permanently to stdout scrollback. Plane B (Composer/Status) is ephemeral and pinned to the bottom.

**Rationale:**

1. **Native Ergonomics**: Preserves terminal-native selection, copy-paste, and search (`Cmd+F`).
2. **Framework Alignment**: Matches the established patterns of `Claude Code`, `Codex`, and `pi-mono`.
3. **Simplicity**: Removes the complexity of managing a virtual scroll buffer within the TUI.

**Tradeoffs:**

- Resizing old scrollback content is impossible (handled by terminal rewrap).
- Requires careful ANSI sequence management to prevent "ghost" lines during redraw.

---

## 2026-03-24 — Configuration: Use TOML for the user-facing source of truth

**Context:** Ion needs a small, human-editable config file that stores the active provider/model selection and can be updated by TUI actions. The config is user-facing, not machine-generated state.

**Decision:** Make `~/.config/ion/config.toml` the user-facing source of truth for provider/model configuration, and keep ion-owned runtime state in `~/.ion/state.toml`.

**Rationale:**

1. **Human-friendly:** TOML is easier to read and edit than JSON for a tiny config file.
2. **Comment-friendly:** Users can annotate local setup without breaking the format.
3. **Rust parity:** Matches the style of the Rust-era workflow and keeps the config story consistent across the archive/reference material.
4. **Go simplicity:** The loader/writer remains small and explicit; no need for a heavier config system.

**Tradeoffs:** Requires migrating the current JSON prototype and updating any code that still assumes `.json`. Legacy `~/.ion/config.toml` remains as a compatibility source during the transition, but new writes go to the XDG settings path.

---

## 2026-03-24 — TUI: Resume progress restoration instead of synthetic status banners

**Context:** The progress line is a live status surface, not a lifecycle notification area. On session resume, the user should see the last meaningful progress text when possible.

**Decision:** When resuming or continuing a session, restore the prior progress line content from session/backend state instead of printing a synthetic "session restored" message.

**Rationale:**

1. **Truthful UX:** The progress line should reflect actual work/status, not a generic banner.
2. **Continuity:** Users can immediately see what the agent was doing before the session was resumed.
3. **Consistency:** Keeps the top status surface aligned with the rest of the event-driven UI.

**Tradeoffs:** Requires preserving or deriving progress text from session/backend state across resume paths.

---

## 2026-03-23 — Permissions: Unified Policy Engine for Host-Level Gating

**Context:** With the addition of ACP support, external agents can now request sensitive operations. Ion needs a unified way to gate these regardless of the backend (Native vs. ACP).

**Decision:** Implement a central Policy Engine in the Host layer. Backends must emit standardized `ApprovalRequest` events, and the Host decides based on user configuration and session state.

**Rationale:**

1. **Security**: Ensures Ion remains the ultimate gatekeeper for the local environment.
2. **Consistency**: Provides a uniform UX for approvals, whether the agent is built-in or external.
3. **Granularity**: Allows for future trust-level configurations (e.g., "always allow read", "ask for write").

**Tradeoffs:** Requires backends to strictly adhere to the `ApprovalRequest` protocol.

---

## 2026-03-24 — ACP: Migrate from custom protocol to real JSON-RPC 2.0 via acp-go-sdk

**Context:** The initial ACP implementation used a hand-rolled `{"type":"...","data":{}}` protocol over stdio. This was not spec-compliant and diverged from the official ACP SDK, making interop with real agents (claude, gemini, gh) impossible.

**Decision:** Rewrite `internal/backend/acp/` to use `github.com/coder/acp-go-sdk` (Apache 2.0). Ion speaks real ACP JSON-RPC 2.0 as a client; external CLI agents are ACP agents.

**Rationale:**

1. **Real interop**: Enables ion to work with actual `claude --acp`, `gemini --acp`, and `gh copilot --acp` binaries without any shim.
2. **SDK guarantees**: Protocol correctness, session lifecycle, and handshake handled by the SDK.
3. **Test infra**: `acp.NewAgentSideConnection` allows in-process mock agents for unit tests without spawning real processes.

**Tradeoffs:** Ties ion to the Coder SDK's API surface. Protocol extensions (e.g. token usage) require checking per-agent `_extension` notifications rather than a standard field.

---

## 2026-03-25 — Backend: Two backends only — canto (API key) and acp (subscription)

**Context:** Ion had three backend packages: `canto`, `acp`, and `native`. The `native` backend was a 204-line Gemini-only SDK prototype, never architecturally correct, and incompatible with the canto-based session model.

**Decision:** Remove `internal/backend/native/`. Ion has exactly two backends: `canto` (API key → direct provider API) and `acp` (subscription → spawn CLI via ACP). No `--backend` flag exposed to users.

**Rationale:**

1. **Clarity**: Two well-defined backends with distinct access models — no overlap, no ambiguity.
2. **ToS compliance**: API key users go through canto; subscription users must go through their provider's CLI (ACP is the only compliant path — Google and Anthropic explicitly prohibit subscription OAuth tokens against their APIs).
3. **UX simplicity**: Users select a provider name (`claude-pro`, `anthropic`, etc.); the backend is derived automatically. ACP is never exposed as a concept.

**Tradeoffs:** Subscription providers require the official CLI to be installed. ion cannot fall back to direct API if the CLI is absent.

---

## 2026-03-27 — Model catalogs: direct native provider fetchers first, optional catwalk only when configured

**Context:** Ion initially relied on a mix of OpenRouter direct fetches and catwalk-based fallback for most other providers. That left the native model picker dependent on `CATWALK_URL` for providers ion already supported directly, and it created two inconsistent rules for model-list and metadata lookup.

**Decision:** Use direct provider fetchers for every native provider (`anthropic`, `openai`, `openrouter`, `gemini`, `ollama`) and treat catwalk as an explicit optional remote catalog only when `CATWALK_URL` is set.

**Rationale:**

1. **Single source-of-truth rule:** The native provider picker and metadata lookup now follow the same policy.
2. **No localhost assumptions:** Ion no longer depends on catwalk’s localhost-oriented default behavior for normal model discovery.
3. **Freshness where it matters:** Providers with official list endpoints, especially OpenRouter, are fetched directly at runtime and cached locally.
4. **Predictable fallback:** The only automatic fallback is the local ion cache; catwalk is opt-in rather than a hidden dependency.

**Tradeoffs:** Some providers still expose only partial metadata through their official list endpoints, so context/pricing completeness can lag behind raw model availability until supplemental metadata is added.

---

## 2026-03-26 — Transcript: Keep runtime switch/resume markers out of durable history

**Context:** Runtime switch notices and resume markers are useful for the host UI and for making session boundaries visible, but they are not transcript messages authored by the user or the model.

**Decision:** Do not persist runtime switch/resume markers as transcript rows. Keep durable storage limited to actual transcript content and explicit runtime/session events, and let the TUI render switch/resume notices live instead of replaying them as history entries.

**Rationale:**

1. **Clear semantics:** transcript history stays about the conversation, not host annotations.
2. **Cleaner storage:** durable records remain aligned with what the model actually said or did.
3. **Better evolution path:** if we later need durable annotations, they can live in a separate event channel without redefining transcript meaning.

**Tradeoffs:** Switch/resume boundaries are no longer queryable as transcript messages; any future persistence should use a dedicated annotation/event type, not the message history table.

---

## 2026-03-25 — Providers: Subscription access via ACP; API key access via canto

**Context:** Users with Claude Pro, Gemini Advanced, or GitHub Copilot subscriptions cannot use their subscription credentials for direct API calls — this violates Anthropic's and Google's Terms of Service. A compliant access path was needed.

**Decision:** Subscription providers (`claude-pro`, `gemini-advanced`, `gh-copilot`) use the ACP backend: ion spawns the official CLI tool and bridges via ACP over stdio. API key providers (`anthropic`, `openai`, `openrouter`) use canto directly. Provider → backend mapping is internal to ion; users never configure a backend directly.

**Rationale:**

1. **ToS compliance**: Spawning the official CLI with ACP lets each provider manage its own auth — ion never touches subscription credentials.
2. **Provider selection UX**: `/provider` command in TUI lists all options (subscription + API key); user picks one; backend is derived.
3. **`ION_ACP_COMMAND` escape hatch**: Advanced users can override the derived CLI command without changing backend selection logic.

**Tradeoffs:** Subscription providers have no standard ACP mechanism for token usage reporting. Codex and ChatGPT Plus ToS status unverified — held back pending confirmation.

---

## 2026-03-25 — Product: ion is a standalone coding agent; ACP is secondary

**Context:** Design docs and agent instructions were framing ACP as equally important to the native path, causing agents to treat ACP feature gaps as blockers and over-invest in ACP infrastructure before the native product was solid.

**Decision:** ion is primarily a standalone coding agent (same category as Claude Code, OpenCode, pi) that talks directly to LLM APIs via canto. ACP is a secondary feature for subscription access only. All new features go into the native path first. ACP bridges them afterward.

**Rationale:**

1. **Product clarity**: the native path is the product. ACP is a legal workaround for a subset of users.
2. **Development velocity**: ACP unknowns (token usage, session resume, feature bridging) should not block native ion progress.
3. **Correct defaults**: when something is unclear, make it work in native mode first.

**Tradeoffs:** ACP users get features later than API key users. Acceptable given the ToS constraint is on their provider's side.

---

## 2026-03-25 — ACP: Goal is seamless parity with native, not best-effort

**Context:** Initial ACP framing used "best-effort" language, which undershot the ambition and gave agents permission to not bridge features.

**Decision:** The goal is for ACP mode to feel as seamless as native ion. The mechanism: expose ion-side capabilities (sub-agents, memory, tools) as ACP-callable tools that the external CLI agent can invoke. Build native features first, then bridge them to ACP.

**Rationale:** Subscription users shouldn't hit a wall of missing features. ACP is a transport concern — ion's feature set should be accessible regardless of backend.

**Tradeoffs:** Feature bridging via ACP tool injection is not yet designed in detail. Figure out as each native feature matures.

---

## 2026-03-25 — ACP: ion should also implement the ACP agent interface

**Context:** Ion currently implements the ACP client (host) side only. If ion also implemented the ACP agent interface, other hosts (IDEs, orchestrators, future tools) could spawn ion as a subprocess and drive it via ACP.

**Decision:** Ion should support an ACP agent mode — a headless path where it implements `acp.Agent` and receives prompts from an external host. Entry point: `--agent` flag or `cmd/ion-agent`. The native canto agent maps cleanly onto the ACP agent interface (`Prompt` → canto turn, session events → `SessionUpdate` notifications).

**Rationale:**

1. **Composability**: ion becomes an interchangeable agent service, not just a TUI app.
2. **Ecosystem alignment**: ACP is heading toward agents-as-services.
3. **Low incremental cost**: `acp-go-sdk` already has `NewAgentSideConnection` (used in tests).

**Tradeoffs:** Headless mode needs a non-TUI approval UX. Build after native path is solid. Tracked as tk-st4q.

---

## 2026-03-25 — Providers: gh-copilot and chatgpt-plus likely allow OAuth, verify before switching

**Context:** OpenAI and GitHub appear to be supportive of coding tools (OpenCode, ion) using OAuth for subscription access, unlike Anthropic and Google which explicitly prohibit it.

**Decision:** Keep gh-copilot and chatgpt-plus on ACP for now — it's safe and works regardless. Before implementing an OAuth/canto path for these providers, verify their ToS explicitly. If confirmed, they can route via canto directly (simpler, more native features).

**Rationale:** ACP works for all subscription providers. The cost of being wrong about OAuth is ToS violation. Verify first.

---

## 2026-03-25 — TUI: Startup header is two lines, not one

**Context:** The header was rendering `ion v0.0.0 · ~/path · branch` on a single line — visually dense and inconsistent with the Rust-era design which put `ion vX.Y.Z` on line 1 and the directory on line 2.

**Decision:** Split the header into two lines: line 1 = `ion vX.Y.Z`, line 2 = `~/path · branch`. The branch appears on line 2 (not in the Rust version, but added because it's immediately useful context).

**Rationale:** Two-line layout matches the Rust reference, reduces visual density, and makes the tool name stand out as a clear anchor.

---

## 2026-03-25 — TUI: Log backend/provider/model to scrollback at startup

**Context:** `boot.Status` (e.g. `"Connected to gemini via Canto"`) was stored in the model and shown in the status bar only. The status bar is ephemeral — truncated, obscured, or overwritten during a session.

**Decision:** Log `boot.Status` to scrollback via `tea.Printf` in `Init()` when non-empty. This gives a persistent record of which backend/provider/model was active when the session started.

**Rationale:** On long sessions or after resume, a quick scroll reveals exactly what was running. The status bar is for live state; scrollback is for session history.

---

## 2026-03-25 — TUI: Workspace metadata owned by main.go, not model.go

**Context:** `currentWorkdir()` and `currentBranch()` were defined in both `cmd/ion/main.go` and `internal/app/model.go` — identical duplication. `main.go` already had `cwd` and was passing it to `store.OpenSession()`.

**Decision:** Remove both helpers from `model.go`. Change `app.New()` to accept `workdir, branch, version string`. `main.go` supplies all three. Version is set via `-ldflags "-X main.version=vX.Y.Z"` at build time, defaulting to `"dev"`.

**Rationale:** Workspace metadata is startup context, not TUI logic. Derived once in main.go and passed in. Improves testability — tests can pass explicit values without touching the filesystem.

---

## 2026-04-02 — TUI: avoid function keys and keep model selection on slash commands

**Context:** ion currently uses `Ctrl+P` for the primary/fast swap and `Ctrl+T` for thinking. Function keys and extra direct hotkeys for model lanes add conflict pressure against standard terminal editing keys without improving the core flow.

**Decision:** Do not add function keys or more direct `Ctrl+<letter>` bindings for model speed lanes. Keep explicit model selection on `/model`. The model picker should own provider, model, and favorites scopes; `primary` and `fast` are the UI-visible presets, with any additional presets remaining config-only or picker-local.

**Rationale:** Terminal portability matters, but the more important point is reducing the global keymap. A single runtime picker keeps the control surface small, preserves more composer editing behavior, and maps well to Bubble Tea's modal overlay model.

---

## 2026-03-27 — TUI: Startup and resume banners stay provider/model-free

**Context:** The startup banner and initial connection notice are printed once into native terminal scrollback. Provider/model metadata in those lines became stale after runtime switches because the footer is the only live-updating source of truth.

**Decision:** Keep startup and resume banners runtime-only (`ion vX.Y.Z • native|acp`) and keep the connection notice provider/model-free (`Connected via Canto` / `Connected via ACP`).

**Rationale:** One-time scrollback lines must not pretend to be live state. The footer already carries the current provider/model, so removing provider/model from startup output avoids stale metadata without reintroducing fragile dynamic startup rendering.

---

## 2026-03-27 — Session naming: main role is agent, child role is subagent

**Context:** Ion was still using `assistant` terminology in parts of the session/event model while also using `agent` for child-agent output. That was both inconsistent with current coding-agent conventions and semantically overloaded.

**Decision:** Rename the main runtime role and event language to `agent` (`Agent`, `AgentDelta`, `AgentMessage`) and rename the child-agent transcript role to `subagent`.

**Rationale:** `agent` matches the product category and removes the collision where `agent` previously meant only child-agent transcript rows. External dependency boundaries may still expose provider-native `assistant` roles, but ion maps them into `agent` at the adapter boundary.

---

## 2026-03-25 — TUI: Mode indicator stays in status bar, not punched into separator

**Context:** The `config-and-metadata.md` spec said the `[WRITE]/[READ]` mode indicator should be "punched into" the top separator bar. The Rust-era design put it in the status bar. The current Go implementation also puts it in the status bar.

**Decision:** Keep mode indicator in the status bar. The spec was aspirational and the implementation that followed matched the Rust design, not the spec. The separator version looked cluttered.

**Rationale:** Clean separator is visually cleaner. Mode is clearly visible in the status bar at a glance. Update spec to match reality.

---

## 2026-03-25 — TUI: picker overlays needed for provider/session discovery

**Context:** ion has `/model` and `/provider` slash commands, but users must know the exact name to type. OpenRouter has 200+ models. Without a picker, explicit provider/model selection is inaccessible.

**Decision:** Add picker overlays for provider selection and `/resume`, rendered in Plane B with ESC to dismiss and Enter to select. Explicit model selection stays on `/model` and the picker flow it opens, with fuzzy search inside the picker rather than a separate global model hotkey.

**Rationale:** This is the gap between "technically works" and "actually usable." Claude Code, OpenCode, and pi all have selection surfaces. Without it, ion is a CLI-only workflow.

**Tradeoffs:** Selection is text-led rather than hotkey-led, but the control surface stays smaller and the picker implementation stays reusable.

---

## 2026-03-25 — TUI: `/model` and `/provider` reopen a fresh runtime on switch

**Context:** The slash commands initially only persisted config changes. That updated the user config file but left the running backend/session on the old provider/model until the process restarted.

**Decision:** When `/model` or `/provider` succeeds, close the current session and open a fresh backend/session pair from the updated config. The new runtime becomes the active model immediately, and the TUI swaps to the new backend/session handles.

**Rationale:** Runtime switching should be truthful and explicit. Reopening a fresh runtime keeps the implementation simple, avoids brittle in-place backend mutation, and makes the session transition easy to reason about.

---

## 2026-03-25 — Configuration: runtime state lives in `~/.ion/state.toml`

**Context:** The old config surface mixed user-editable values with runtime-owned provider, model, and storage settings. That made the file noisy and forced the app to treat hand-edited config as mutable state.

**Decision:** Treat `~/.ion/state.toml` as the runtime source of truth for provider, model, context limit, data dir, cache TTL, and session retention. Reserve `~/.ion/config.toml` for future hand-edited user settings and do not rely on it for runtime selection. Do not add legacy fallback or migration paths between the two.

**Rationale:**

1. **UX clarity**: runtime state is machine-owned, so it should not be mixed into the user settings file.
2. **Startup truthfulness**: ion can populate its own runtime file on startup and runtime commands can update it directly.
3. **No compatibility debt**: v0.0.0 does not need migration shims or compatibility fallbacks.

**Tradeoffs:** `state.toml` is now an implementation-owned file that ion is expected to update directly when provider/model changes.

---

## 2026-03-26 — TUI: provider/model switches preserve the active session

**Context:** Mid-conversation provider/model swaps should behave like a handoff, not a fork. The user expects the same conversation history, tool context, and session boundary to continue when they change models, even if the backend changes.

**Decision:** When `/provider`, `/model`, or the picker flow changes the runtime, reopen the new backend against the current session ID instead of creating a new session. Record an explicit transcript notice for the switch so the change is visible in history.

**Rationale:** This keeps the session continuous and makes provider/model changes part of the same conversation instead of a detached new thread. It also matches the handoff model used by multi-model agents: preserve serializable context, transform what must be transformed, and avoid surprising session forks.

**Tradeoffs:** Session metadata may lag behind the current runtime until we decide whether to surface the latest active model separately from the session's original creation model.

---

## 2026-03-25 — Providers: `chatgpt` is the canonical OpenAI subscription name

**Context:** The Rust archive used `chatgpt` as the OpenAI OAuth/subscription provider name. The Go tree briefly introduced `chatgpt-plus`, which added a new name that did not exist in the reference implementation.

**Decision:** Use `chatgpt` as the canonical provider name for OpenAI subscription access. Bridge it via the local `codex --acp` CLI when ACP is available. Do not use `chatgpt-plus` as a provider name.

**Rationale:**

1. **Archive parity**: matches the Rust-era naming.
2. **Less ambiguity**: one provider name instead of two overlapping ChatGPT labels.
3. **Operational fit**: the local Codex CLI is the ACP bridge currently available on this machine.

**Tradeoffs:** OpenAI subscription access remains an ACP path and still depends on the CLI bridge being installed and functional.

## 2026-03-25 — Configuration: keep user intent in `~/.ion/config.toml`, not ion-owned state

**Context:** The `state.toml` split pushed provider/model/session retention into machine-owned state and required ion to write runtime selection on startup. That made user intent harder to inspect, broke the expected hand-edited workflow, and added a second config surface without a real machine-owned need.

**Decision:** Keep provider, model, optional context override, and session retention in `~/.ion/config.toml`. Do not auto-write provider/model on startup. Hardcode ion-owned defaults such as data dir and model metadata cache TTL. Sessions, transcripts, and knowledge continue to persist in SQLite under `~/.ion/data`.

**Rationale:**

1. **User ownership:** Provider/model choice is user intent, so it belongs in the hand-edited config file and in TUI-driven settings writes.
2. **Cleaner startup:** Ion should read config, not rewrite it just to start.
3. **Less surface area:** A second state file is unnecessary until there is a real machine-owned value that cannot live in code or the existing database.
4. **Truthful persistence:** Session-like runtime data already has a natural home in SQLite instead of an ad hoc TOML state file.

**Tradeoffs:** If ion later needs machine-owned persisted values outside the database, add a dedicated state file then, but not preemptively.

---

## 2026-03-30 — Approval: 3-mode permission system (READ/EDIT/YOLO)

**Context:** Ion had 2 modes (READ/WRITE) where WRITE gated edits/bash with a safe-command whitelist and READ only blocked mutations. Research across 7 agents (Claude Code, Codex CLI, Gemini CLI, OpenCode, Pi, Zed, ACP) showed industry convergence on: `yolo` naming, read tools never gated, "always allow" at approval time as biggest UX win, sandbox orthogonal to approval.

**Decision:** Replace READ/WRITE with 3 modes:
- **READ**: look-only, bash entirely blocked, MCP prompted
- **EDIT**: prompts for all mutations and execution, default startup mode
- **YOLO**: auto-approve everything

`Shift+Tab` cycles READ → EDIT → YOLO → READ. `/yolo` toggles on/off. Approval prompt gains `a` key for session-scoped category auto-approve. Config: `default_mode = "edit"` in `~/.ion/config.toml`.

**Rationale:**

1. 3 clean modes with one semantic jump between each — no overlap
2. READ blocking bash entirely (not safe-listing) prevents LLM permission escapes via crafted commands
3. EDIT as default matches industry consensus for daily driving — visibility over speed
4. Session-scoped `a` key handles 90% of approval fatigue without config complexity
5. No plan mode — READ + system prompt instruction covers it later

**Spec:** `ai/specs/tools-and-modes.md`
**Research:** `ai/research/approval-ux-survey-2026-03-30.md`
**Tracked by:** `tk-k4hv`

---

## 2026-03-30 — Architecture: Canto/Ion Alignment for SOTA Maturity

**Context:** As Ion matures toward a SOTA (State of the Art) coding agent, the boundary between the framework (Canto) and the application (Ion) needed sharpening. Universal needs like safety policies, standard tools, and context management were being re-implemented at the application level.

**Decision:** Formally align the architecture into Layer 3 (Canto Framework) and Layer 4 (Ion Application).
- **Canto (L3)**: Owns the `PolicyEngine`, standard coding tools (`bash`, `file`, `grep`), and a background "Context Governor" for automated compaction.
- **Ion (L4)**: Owns the TUI (Bubble Tea), workspace metadata, and UX-level policy definitions.

**Rationale:**
1. **Universal Safety**: Moving the `PolicyEngine` to Canto ensures that all agents built on the framework are "Secure by Default."
2. **Golden Tools**: Standardizing `bash` and `file` tools in `canto/x/tools` provides audited, secure implementations for all consumers.
3. **Infinite Context**: Framework-level context governance makes "infinite memory" a foundation feature rather than a TUI-driven hack.
4. **Thin UI**: Ion becomes a pure interface, making it easier to port to other platforms (CLI, Web).

**Tracked by:** `tk-f1cn`, `tk-canto-refactor`, `tk-canto-governor`

---

## 2026-03-30 — TUI: Componentized Sub-Model Architecture

**Context:** The main `Model` struct in `internal/app/model.go` had become a "God Object," handling rendering, input history, status bar logic, and backend event translation. This made it difficult to test and maintain.

**Decision:** Refactor the TUI into isolated sub-components: `Viewport`, `Input`, `Broker`, and `Util`.

**Rationale:**
1. **Separation of Concerns**: Rendering logic (`Viewport`) is separated from user interaction (`Input`) and backend communication (`Broker`).
2. **Maintainability**: Smaller files are easier to navigate and reason about.
3. **Testability**: Components can eventually be tested in isolation without initializing the entire app state.
4. **Idiomatic Go**: Leveraging separate files within the `app` package follows standard Go project organization.

**Tracked by:** `tk-2b79`
