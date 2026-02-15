# ion Decisions

> Decision records for ion development. Pre-February decisions archived in `ai/DECISIONS-archive-jan.md`.

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

| Type      | Context      | Examples                      |
| --------- | ------------ | ----------------------------- |
| Skills    | Same context | developer, designer, refactor |
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
