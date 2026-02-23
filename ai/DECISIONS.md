# ion Decisions

> Decision records for ion development. Pre-February decisions archived in `ai/DECISIONS-archive-jan.md`.

## 2026-02-23: Elm-Style App+Effect over React Hooks for `crates/tui`

**Context**: rnk uses a React hooks model (`use_state`, `use_effect`). Considered adopting the
same pattern for `crates/tui`.

**Decision**: Use Elm-style `App` trait + `Effect` system instead.

**Rationale**: React hooks get unwieldy at scale — LLM streaming involves a continuous byte
stream, partial tool call JSON accumulating, multiple concurrent tool executions, and mutable
conversation history. Mapping that onto `use_state`/`use_effect` creates impedance mismatch.
Elm-style `update()` is pure, side effects go through `Effect`, state mutation is centralized.
More testable, scales better to ion's complexity. Bubbletea (Go) uses this model successfully
for similar workloads.

---

## 2026-02-22: Build `crates/tui/` as a General-Purpose Library

**Context**: ion's TUI was using rnk only for the bottom UI bar. rnk has been removed and replaced
with direct crossterm calls. Evaluated whether to continue incrementally cleaning up ion's TUI or
build a proper general-purpose library.

**Decision**: Build `crates/tui/` as a standalone general-purpose TUI library crate (workspace
member), with ion as the first consumer. Two-layer architecture:

- `crates/tui/` — generic library, zero ion knowledge; ion depends on it as a path dep
- `src/ui/` — agent-specific widgets (`StreamingText`, `ConversationView`, `ToolCallView`, etc.)
  built on `crates/tui`'s `Canvas` escape hatch; live in ion, not the library

The library is designed to eventually be publishable independently. Ion drives the design during
Phase 1; it won't be over-engineered for hypothetical future consumers.

**Rationale**: ion needs maybe 6–8 custom widgets regardless of framework. The hard infrastructure
(Taffy layout, tokio event loop, inline mode cursor math, double-buffered diff) is worth owning.
rnk is young with no community; bugs become ion's problem. Owning the render loop means surgical
per-widget dirty tracking at high token throughput.

**Architecture**: Cell-based `Buffer`, Taffy layout, `App` trait + `Effect` system (Elm-style),
`Element`/`Widget` tree. Full inline + fullscreen mode. Spec in `ai/design/tui-lib-spec.md`.

---

## 2026-02-22: rnk Removed; crossterm-direct + ansi module

**Context**: ion used rnk only to apply ANSI escape codes — running a full flexbox layout engine
to get back a single styled string. The `.lines().next()` usage discarded all multi-line output.

**Decision**: Remove rnk entirely. Replace with `src/tui/ansi.rs` — a thin wrapper over
`crossterm::style::ContentStyle` / `StyledContent<D: Display>`.

**Rationale**: crossterm is already in the dependency tree. `ContentStyle::apply()` implements
`Display` correctly with proper reset handling. 20 lines of wrapper replaces a layout engine
dependency. No layout was needed — only styling.

---

## 2026-02-22: No OS-Level Bash Sandbox

**Context**: Evaluated macOS Seatbelt (`sandbox-exec`) + Linux Landlock for restricting
bash child processes. Implemented, reviewed, then discarded.

**Decision**: Do not implement OS-level bash sandboxing. Keep existing guards only:
`analyze_command()` (destructive pattern blocking) and `check_sandbox()` (app-level
path enforcement for file tools).

**Rationale**: OS sandbox breaks common dev workflows — `cargo build` fails when
fetching new deps (writes to `~/.cargo/registry/`), same for npm/pip caches. Linux
requires a co-installed helper binary (`ion-sandbox`) with its own install story.
`sandbox-exec` is deprecated on macOS. The marginal protection over existing guards
doesn't justify the UX cost. Per badlogic (pi): once an agent can write+exec code,
filesystem sandboxing is a speed bump not a wall.

---

## 2026-02-13: Keep Header Static; Move Location to Status Line

**Context**: Dynamic startup header (including git branch lookup/rendering) increased resize/reflow complexity and contributed to visual duplication/noise during monitor/terminal churn.

**Decision**: Keep startup header static (`ion vX` + cwd only). Display dynamic location context (`cwd [branch]`) in the bottom status line instead.

**Rationale**: Separating static transcript header content from dynamic runtime status reduces reflow churn and keeps chat history cleaner while preserving branch visibility where it matters during active interaction.

---

## 2026-02-13: RNK Line Rendering Must Trim Trailing Padding

**Context**: RNK text line rendering used `render_to_string_no_trim`, which preserves right-padding spaces. During resize/reflow this padding leaked into scrollback and caused malformed wraps/indentation artifacts.

**Decision**: Use `rnk::render_to_string` for line rendering helpers in TUI paths so trailing padding is trimmed before writing to terminal scrollback.

**Rationale**: Trimmed output preserves expected terminal wrap semantics and avoids right-edge padding side effects during viewport resize and monitor switches.

---

## 2026-01-27: TUI v2 - Drop ratatui, Use crossterm Directly

**Context**: `Viewport::Inline(15)` creates a fixed 15-line viewport. Our UI needs dynamic height. Research showed Codex CLI doesn't use `Viewport::Inline` either.

**Decision**: Remove ratatui entirely, use crossterm for direct terminal I/O.

| Component    | Before (ratatui)                | After (crossterm)                  |
| ------------ | ------------------------------- | ---------------------------------- |
| Chat history | `insert_before()` to scrollback | `println!()` to stdout             |
| Bottom UI    | Fixed `Viewport::Inline(15)`    | Cursor positioning, dynamic height |
| Widgets      | Paragraph, Block, etc.          | Direct ANSI/box-drawing characters |

**Rationale**: ratatui's Viewport::Inline doesn't support dynamic height. Our actual needs (styled text, borders, cursor positioning) are simple enough that crossterm alone suffices.

---

## 2026-01-23: Custom Text Entry with Ropey

**Context**: rat-text, tui-textarea, tui-input were either over-engineered or missing critical features. We need full control over input handling.

**Decision**: Build custom text entry using ropey as the text buffer backend. Direct crossterm events. Codex CLI takes this same approach.

---

## 2026-01-20: License - PolyForm Shield

**Decision**: License under PolyForm Shield 1.0.0. Keeps code public for individual/OSS use while reserving commercial competitive use.

---

## 2026-01-19: Config Priority - Explicit Config > Env Vars

**Decision**: Config file takes priority over environment variables. If a user explicitly puts a key in `~/.ion/config.toml`, that's intentional.

---

## 2026-01-19: Provider-Specific Model IDs

**Decision**: Store model IDs as each provider expects them. No normalization. OpenRouter uses `org/model`, direct providers use native names. Switching providers requires re-selecting model.

---

## 2026-01-18: ContextManager & minijinja Integration

**Decision**: Decouple prompt assembly into `ContextManager` using `minijinja` templates. Stabilize system prompt; inject memory as `User` message for cache-friendliness.

---

## 2026-01-16: Sub-Agents vs Skills Architecture

**Decision**: Sub-agents for context isolation; skills for behavior modification.

| Type       | Context      | Examples                       |
| ---------- | ------------ | ------------------------------ |
| Skills     | Same context | developer, designer, refactor  |
| Sub-agents | Isolated     | explorer, researcher, reviewer |

Binary model choice: fast (explorer only) vs full (everything else).

---

## 2026-01-18: Plugin Architecture (Claude Code Compatible)

**Decision**: Claude Code-compatible hook system. Hook events: SessionStart, SessionEnd, UserPromptSubmit, PreToolUse, PostToolUse, PreCompact, Stop, Notification.

---

## 2026-01-14: Async TUI-Agent Communication

**Decision**: `tokio::sync::mpsc` channels. Agent spawns in background, sends `AgentEvent` to channel. TUI polls in every update loop.

---

## 2026-01-14: Double-ESC Cancellation

**Decision**: Double-press ESC within 1.5s to cancel. Avoids accidental triggers from single-key cancel.
