# TUI Libraries for Hybrid Chat (Scrollback + Bottom UI) -- 2026-02

**Research Date:** 2026-02-09
**Question:** Is there a library that helps with the hybrid model (scrollback chat + positioned bottom UI)?
**Prior research:** tui-state-of-art-2026.md, ratatui-vs-crossterm-v3.md, tui-v2 design doc

---

## Answer

**No library exists for this specific pattern.** Custom abstractions over crossterm remain the correct approach. Ion's current TUI v2 architecture (raw crossterm, ScrollUp for scrollback, cursor-positioned bottom UI, BeginSynchronizedUpdate) is well-aligned with the state of the art.

---

## 1. Library Evaluation

### Ratatui Viewport::Inline -- Closest Match, Still Inadequate

The inline viewport is the closest existing abstraction. Since the Jan 2026 research:

| Feature                     | Status (Feb 2026)                          | Ion Implication                           |
| --------------------------- | ------------------------------------------ | ----------------------------------------- |
| `scrolling-regions` feature | Stable in v0.29+, opt-in                   | Would fix insert_before flicker           |
| Dynamic viewport height     | PR #1964 still not merged                  | Ion needs dynamic height for input growth |
| Horizontal resize           | Still broken (Issue #2086, Draft PR #2355) | Ion handles this with full-reprint        |
| `insert_lines_before`       | Issue #1426 open, not implemented          | Would help but not available              |
| `set_viewport_height`       | PR #1964 open                              | Would be needed for dynamic input         |

**Verdict:** Ratatui Viewport::Inline cannot replace ion's current approach. The fixed viewport height is the deal-breaker -- ion's input box grows/shrinks and the progress line appears/disappears. Even if `set_viewport_height` merged, the horizontal resize problem remains.

**What ratatui COULD provide:** Widget rendering to a Buffer (borders, styled text, layout). This is a la carte -- you can use `ratatui-core` and `ratatui-widgets` without the Terminal/Viewport abstraction. But ion already has its own styled output and does not need ratatui's widget system for 3 fixed-position lines.

### Lightweight Crossterm Wrappers

| Crate               | What It Does                                  | Viable?                                                        |
| ------------------- | --------------------------------------------- | -------------------------------------------------------------- |
| `crossterm-display` | Performance wrapper for crossterm writes      | Minimal value -- ion already uses queue!/execute!              |
| `console_engine`    | Frame-based screen abstraction over crossterm | Full-screen ownership assumed, no hybrid mode                  |
| `terminal` (crate)  | Backend-agnostic terminal API                 | Abstraction layer adds indirection, no hybrid features         |
| `termwiz`           | WezTerm's terminal lib                        | Low-level like crossterm, different API, no hybrid abstraction |

**Verdict:** None of these provide the hybrid model. They are either too thin (crossterm-display) or too opinionated about owning the full screen (console_engine).

### iocraft

React-like declarative TUI. Uses flexbox layout via taffy. Newer, smaller ecosystem. Does not have an inline/hybrid mode -- it is designed for full-screen applications like ratatui.

### Cursive

Dialog/view-based TUI. Full-screen ownership. Not suitable for the scrollback + bottom UI pattern.

---

## 2. How Other Chat TUIs Handle This

### Codex CLI (OpenAI) -- Rust/ratatui

**Most relevant reference.** Key developments since Jan 2026:

- **TUI2 was abandoned** (PR #9640): They tried a fullscreen mode with in-memory scrollback management. Reverted to inline terminal-native approach because "terminal functionality that works uniformly everywhere trumps sophisticated but environment-specific features."
- **Current approach:** Custom terminal wrapper (not Viewport::Inline), scroll regions (DECSTBM) for inserting history lines, ratatui widgets for rendering content to buffers.
- **Still uses ratatui** but only for: Buffer/Rect/widget rendering. The Terminal/Viewport layer is custom.

### aichat (sigoden) -- Rust/crossterm + reedline

**Not ratatui.** Uses crossterm 0.28 directly plus reedline for line editing. This is a REPL-style interface, not a managed TUI:

- Standard terminal output for responses
- reedline handles input with tab completion, history, multi-line
- No managed viewport, no bottom UI area
- Native scrollback for everything

**Lesson:** For a pure chat REPL, reedline + raw output is sufficient. But ion needs a managed bottom UI (status, progress, input), which reedline does not provide.

### tenere -- Rust/ratatui (full-screen)

Uses ratatui in standard fullscreen mode (alternate screen). Chat scrollback is managed in-memory with a scrollable widget. No native terminal scrollback.

### oatmeal -- Rust/ratatui (full-screen)

Same pattern as tenere. Full alternate screen, ratatui widgets for everything.

### Claude Code -- TypeScript/React

Shipped a custom differential renderer (Jan 2026). Key insights from the HN thread:

- Rewrote rendering from scratch, ~1/3 of sessions still see at least one flicker
- Working upstream on synchronized output (DEC 2026h) support in VSCode terminal and tmux
- The differential renderer approach reduces scroll events from 4000-6700/sec to near-zero

**Lesson:** Even with major investment, inline terminal rendering has inherent flicker challenges. Synchronized output is the primary mitigation.

### Gemini CLI -- TypeScript

Uses alternate screen by default. Prints transcript to scrollback on exit. Clean approach that avoids all inline rendering challenges.

---

## 3. What Ion Already Does Right

Ion's TUI v2 (already implemented) uses the exact techniques the ecosystem has converged on:

| Technique                                  | Ion Status | Industry Reference            |
| ------------------------------------------ | ---------- | ----------------------------- |
| Raw crossterm (no ratatui viewport)        | Done       | Codex custom terminal, aichat |
| ScrollUp for scrollback insertion          | Done       | Codex insert_history_lines    |
| BeginSynchronizedUpdate/End wrapping       | Done       | Claude Code, pi-mono          |
| Row-tracking mode (content fits on screen) | Done       | Unique to ion                 |
| Scroll mode (content exceeds screen)       | Done       | Standard pattern              |
| Cursor-positioned bottom UI                | Done       | Codex, pi-mono                |
| Full reprint on resize                     | Done       | Codex, Claude Code            |

### What Could Be Improved

| Area                             | Current                   | Potential Improvement                                                                             |
| -------------------------------- | ------------------------- | ------------------------------------------------------------------------------------------------- |
| Scroll regions (DECSTBM)         | Not used -- uses ScrollUp | DECSTBM could scroll just the area above the UI, avoiding bottom UI flicker during chat insertion |
| Line-level diffing for bottom UI | Clear + full redraw       | Only redraw changed lines. But at 3-5 lines, the benefit is marginal.                             |
| Streaming response in-place      | Managed area rendering    | Already working per render_state.rs                                                               |

---

## 4. Should Ion Adopt DECSTBM Scroll Regions?

### What It Would Buy

Currently, inserting chat content uses `ScrollUp(n)` which scrolls the **entire screen** including the bottom UI. The bottom UI must then be fully redrawn. With DECSTBM:

```
1. Set scroll region to rows 0..(term_height - ui_height)
2. Move cursor to bottom of scroll region
3. Print new chat lines (region scrolls automatically)
4. Reset scroll region
5. Bottom UI is untouched -- no redraw needed
```

### Trade-offs

| Pro                                                   | Con                                                          |
| ----------------------------------------------------- | ------------------------------------------------------------ |
| Bottom UI never flickers during chat insertion        | More complex escape sequence management                      |
| Fewer bytes written per chat update                   | Must always reset scroll region (or terminal breaks on exit) |
| Matches Codex's production-proven approach            | Some terminals may not support DECSTBM correctly             |
| Could skip bottom UI redraw entirely during streaming | Crossterm does not expose DECSTBM -- must write raw ANSI     |

### Implementation Difficulty

Low-medium. crossterm 0.29 added `scroll_region_up`/`scroll_region_down` in PR #918 (the same primitives ratatui's `scrolling-regions` feature uses). However, these are not yet in crossterm's stable public API -- they were added for ratatui's backend use.

Custom DECSTBM commands are straightforward:

```rust
struct SetScrollRegion { top: u16, bottom: u16 }
impl crossterm::Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.top + 1, self.bottom + 1)
    }
}

struct ResetScrollRegion;
impl crossterm::Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[r")
    }
}
```

### Recommendation

**Not urgent.** Ion already uses `BeginSynchronizedUpdate` which prevents visible flicker during the ScrollUp + bottom UI redraw cycle. DECSTBM would be a refinement -- fewer bytes on the wire, theoretically smoother. Worth considering if flicker is observed in practice, particularly in terminals that do not support synchronized output (though most modern terminals do).

---

## 5. Summary

| Question                                              | Answer                                                                                                                                                      |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Is there a library for hybrid scrollback + bottom UI? | **No.**                                                                                                                                                     |
| Should ion adopt ratatui (inline or otherwise)?       | **No.** Ratatui's inline viewport has the same limitations ion already solved.                                                                              |
| Should ion adopt any lightweight crossterm wrapper?   | **No.** The wrappers add indirection without solving the hybrid problem.                                                                                    |
| Are custom abstractions the right answer?             | **Yes.** This is what Codex, Claude Code, and pi-mono all concluded independently.                                                                          |
| What is the main potential improvement?               | **DECSTBM scroll regions** to avoid redrawing the bottom UI during chat insertion. Low priority given synchronized output already prevents visible flicker. |

---

## Sources

### Primary

- [Ratatui Viewport::Inline docs](https://docs.rs/ratatui/latest/ratatui/enum.Viewport.html)
- [Ratatui scrolling-regions PR #1341](https://github.com/ratatui/ratatui/pull/1341)
- [Ratatui inline viewport flickering Issue #584](https://github.com/ratatui/ratatui/issues/584)
- [Ratatui insert_lines_before Issue #1426](https://github.com/ratatui/ratatui/issues/1426)
- [Ratatui inline resize Issue #2086](https://github.com/ratatui/ratatui/issues/2086)
- [Ratatui set_viewport_height PR #1964](https://github.com/ratatui/ratatui/pull/1964)
- [crossterm region-scrolling PR #918](https://github.com/crossterm-rs/crossterm/pull/918)

### Chat TUI References

- [Codex CLI](https://github.com/openai/codex) -- Rust/ratatui hybrid
- [aichat](https://github.com/sigoden/aichat) -- Rust/crossterm + reedline (no ratatui)
- [tenere](https://github.com/pythops/tenere) -- Rust/ratatui fullscreen
- [oatmeal](https://github.com/dustinblackman/oatmeal) -- Rust/ratatui fullscreen
- [Claude Code flickering fix HN thread](https://news.ycombinator.com/item?id=46699072) -- Differential renderer

### Lightweight Alternatives Evaluated

- [crossterm-display](https://crates.io/crates/crossterm-display)
- [console_engine](https://crates.io/crates/console_engine)
- [iocraft](https://crates.io/crates/iocraft)
- [terminal crate](https://crates.io/crates/terminal)

### Prior Ion Research

- `/Users/nick/github/nijaru/ion/ai/research/tui-state-of-art-2026.md`
- `/Users/nick/github/nijaru/ion/ai/research/ratatui-vs-crossterm-v3.md`
- `/Users/nick/github/nijaru/ion/ai/design/tui-v2.md`
