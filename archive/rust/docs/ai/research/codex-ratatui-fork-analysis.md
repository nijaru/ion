# Codex CLI Ratatui Fork Analysis

How OpenAI's Codex CLI uses, patches, and works around ratatui's inline viewport limitations.

**Date:** 2026-02-08

## Summary

| Aspect                   | Detail                                                                                               |
| ------------------------ | ---------------------------------------------------------------------------------------------------- |
| Base dependency          | ratatui 0.29.0                                                                                       |
| Fork                     | `nornagon/ratatui` branch `nornagon-v0.29.0-patch`                                                   |
| Fork author              | Jeremy Rose (nornagon), OpenAI engineer                                                              |
| Fork changes             | (1) expose `set_viewport_area` as public, (2) bump unicode-width 0.2.0->0.2.1                        |
| Additional customization | `custom_terminal.rs` -- a derived copy of `ratatui::Terminal` with custom viewport management        |
| Also patched             | crossterm (fork), tokio-tungstenite (fork), tungstenite (fork)                                       |
| Ratatui features used    | `scrolling-regions`, `unstable-backend-writer`, `unstable-rendered-line-info`, `unstable-widget-ref` |

Codex did NOT fork ratatui in a major way. They patch one method (`set_viewport_area`) to be public, and then build most of their custom behavior in a **derived Terminal struct** (`custom_terminal.rs`) that lives in their own codebase.

## What the Fork Actually Changes

### 1. `set_viewport_area` Exposed

The upstream ratatui `Terminal` struct has a `viewport_area` field but does not expose a public setter for it. Codex needs to dynamically reposition the viewport (e.g., when the terminal resizes, when switching between inline and alt-screen mode, when the viewport height changes). The fork makes this setter public.

Commit by nornagon (Jeremy Rose), 2025-07-26:

```
expose set_viewport_area
```

### 2. unicode-width Bump

PR #1 by easong-openai, 2025-08-03:

```
Bump unicode-width 0.2.0 to 0.2.1
```

That is it. Two commits on top of the v0.29.0 release.

### 3. Workspace Patch Configuration

From `codex-rs/Cargo.toml`:

```toml
[patch.crates-io]
crossterm = { git = "..." }  # also forked
ratatui = { git = "https://github.com/nornagon/ratatui", branch = "nornagon-v0.29.0-patch" }
tokio-tungstenite = { git = "..." }  # OpenAI fork
tungstenite = { git = "..." }  # OpenAI fork
```

Source: [nornagon/ratatui branch](https://github.com/nornagon/ratatui/tree/nornagon-v0.29.0-patch)

## The Real Customization: `custom_terminal.rs`

The more significant work is **not** in the ratatui fork but in a derived copy of `ratatui::Terminal` that lives at `codex-rs/tui/src/custom_terminal.rs`.

Source: [custom_terminal.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/custom_terminal.rs)

License header:

```
// This is derived from `ratatui::Terminal`, which is licensed under the following terms:
// The MIT License (MIT)
// Copyright (c) 2016-2022 Florian Dehau
// Copyright (c) 2023-2025 The Ratatui Developers
```

### What it adds over upstream Terminal

1. **`set_viewport_area()`** -- Explicit control over the rendering region, can resize both buffers independently
2. **`last_known_cursor_pos`** -- Tracks cursor position for resize heuristics
3. **`last_known_screen_size`** -- Detects terminal resizes
4. **`clear_scrollback()`** -- Uses crossterm `ClearType::Purge` to clear terminal history
5. **Custom viewport positioning** -- Handles inline viewport at arbitrary screen positions rather than requiring full-screen

### Why a derived Terminal was needed

Upstream ratatui `Terminal` assumes:

- Full terminal control (either fullscreen or fixed inline height)
- Single viewport spanning the entire area
- No API for scrollback manipulation
- `viewport_area` is not publicly settable
- Inline viewport height is fixed at creation time (cannot grow/shrink)

Codex needs:

- Dynamic viewport height (grows for streaming content, shrinks for compact input)
- Viewport repositioning on terminal resize
- Switching between inline viewport and alternate screen within the same session
- Scrollback integration (inserting completed messages above viewport)

## How Codex Handles Scrollback + Bottom UI

### Architecture

```
+-- Terminal Screen ------------------------------------+
|                                                       |
| [Scrollback -- managed by terminal emulator]          |
| > Previous user message                               |
| > Previous assistant response                         |
| > Tool output                                         |
|                                                       |
+--- Scroll Region Boundary ---------------------------+
|                                                       |
| [Viewport -- managed by Codex via ratatui]            |
| +---------------------------------------------------+ |
| | Current streaming response...                     | |
| +---------------------------------------------------+ |
| | > Input prompt                                    | |
| +---------------------------------------------------+ |
|                                                       |
+-------------------------------------------------------+
```

### The Two-Phase Model

**Phase 1: Streaming** -- Content renders in the ratatui-managed viewport at the bottom of the screen. The viewport height changes dynamically based on content.

**Phase 2: Completed** -- When a message finishes streaming, it gets "promoted" to terminal scrollback via `insert_history_lines()`. The viewport then shrinks back down to just the input area.

### insert_history_lines Implementation

```rust
pub fn insert_history_lines<B>(
    terminal: &mut Terminal<B>,
    lines: Vec<Line>,
) -> io::Result<()>
where
    B: Backend + Write,
{
    let wrapped = word_wrap_lines_borrowed(&lines, area.width.max(1) as usize);
    // Set scroll region to the area ABOVE the viewport
    queue!(writer, SetScrollRegion(1..area.top()))?;
    queue!(writer, MoveTo(0, cursor_top))?;
    for line in wrapped {
        queue!(writer, Print("\r\n"))?;
        write_spans(writer, merged_spans.iter())?;
    }
    queue!(writer, ResetScrollRegion)?;
    queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;
    Ok(())
}
```

This uses ANSI scroll regions (`\x1b[X;Yr`) to push lines into terminal scrollback without flickering. The viewport stays pinned at the bottom while content scrolls up above it.

### Custom Scroll Region Commands

```rust
pub struct SetScrollRegion(pub std::ops::Range<u16>);
impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }
}
```

### The TUI2 Experiment and Its Failure

Codex tried a "TUI2" approach that took full viewport ownership (alternate screen, managed scrollback, owned selection/copy). It was removed in PR #9640 (2026-01-22) because:

1. **Environment matrix explosion** -- Every combination of terminal emulator, OS, tmux/Zellij, input device, keyboard layout, font broke differently
2. **Scrolling regressions** -- Users reported broken scrollback ([Issue #8344](https://github.com/openai/codex/issues/8344))
3. **Selection/copy failures** -- Native terminal selection stopped working
4. **High CPU** -- Tight render loops caused excessive CPU usage ([Issue #8176](https://github.com/openai/codex/issues/8176))

The team's conclusion: "terminal functionality that works uniformly everywhere trumps sophisticated but environment-specific features."

### The Replacement: Redraw-Based Approach (PR #7601)

After abandoning TUI2, Codex moved to a "redraw-based" model:

1. **In-memory transcript is source of truth** -- Not the terminal scrollback
2. **Display-time wrapping** -- Content wraps at current terminal width, reflows on resize
3. **Content-anchored selection** -- Selection follows transcript content, not screen position
4. **High-water mark printing** -- Each logical line prints to scrollback at most once
5. **Suspend/resume protocol** -- On suspend, print unprinted transcript suffix to scrollback; on resume, re-enter TUI mode and full redraw

This is a hybrid: the transcript is owned in-memory, but scrollback is written to append-only so native terminal scroll/selection/copy still works.

## Known Ratatui Inline Viewport Limitations

### 1. Fixed Height at Creation (Issue #984)

`Viewport::Inline(height)` cannot be dynamically resized. Once created with a height, the viewport stays that size. This is fatal for chat UIs where the viewport needs to grow during streaming and shrink when idle.

Source: [ratatui/issues/984](https://github.com/ratatui/ratatui/issues/984)

### 2. Horizontal Resize Corruption (Issue #2086)

When the terminal shrinks horizontally, wrapped lines cause content to shift. Ratatui continues drawing at the original position, creating misalignment. Fixed in PR #2355 by clearing and repositioning during horizontal resizes.

Source: [ratatui/issues/2086](https://github.com/ratatui/ratatui/issues/2086)

### 3. insert_before Flickering (Issue #584)

The original `insert_before()` clears the viewport area before adding new lines, causing visible flicker during rapid updates. Fixed by the `scrolling-regions` feature (PR #1341) which uses ANSI scroll regions instead of clear+redraw.

Source: [ratatui/issues/584](https://github.com/ratatui/ratatui/issues/584), [ratatui/pull/1341](https://github.com/ratatui/ratatui/pull/1341)

### 4. insert_before Crash on Full Screen

If the inline viewport filled the entire screen, `insert_before()` would crash because there was no area above the viewport to insert into. The reimplementation in the scrolling-regions feature handles this case.

### 5. No insert_lines_before API (Issue #1426)

`insert_before()` accepts a closure that renders to a buffer sized to terminal width. This forces the application to handle wrapping, which means the terminal emulator cannot reflow text on resize. The proposed `insert_lines_before()` would accept raw lines and let the terminal handle wrapping/reflow.

Source: [ratatui/issues/1426](https://github.com/ratatui/ratatui/issues/1426)

### 6. Scrollback Buffer Unreliability

Scrolling regions do not reliably write to the scrollback buffer when lines exit the viewport in all terminal emulators. This creates potential data loss in multiplexers like Zellij.

Source: [codex/issues/6456](https://github.com/openai/codex/issues/6456) (truncated to ~20 lines in Zellij)

### 7. No Signed/Virtual Viewport Coordinates

There is no way to render content partially off-screen (e.g., a block where the first line is above the viewport). This limits smooth scrolling animations and partial visibility effects.

### 8. ScrollbarState Insufficient for Log-Following

`ScrollbarState` does not expose `position` or `viewport_content_length` as readable fields, making it impossible to implement tail/log-following behavior efficiently.

Source: [ratatui/issues/625](https://github.com/ratatui/ratatui/issues/625)

## Ratatui Features Codex Uses

| Feature                       | Purpose                                                  | Status                     |
| ----------------------------- | -------------------------------------------------------- | -------------------------- |
| `scrolling-regions`           | ANSI scroll regions for flicker-free `insert_before`     | Feature-gated, not default |
| `unstable-backend-writer`     | Direct access to backend writer for custom ANSI commands | Unstable                   |
| `unstable-rendered-line-info` | Line rendering metadata for precise viewport control     | Unstable                   |
| `unstable-widget-ref`         | Render widgets by reference (avoids cloning)             | Unstable                   |

The `scrolling-regions` feature was contributed by Neal Fachan (nfachan) in PR #1341, merged October 2024. It adds `scroll_region_up()` and `scroll_region_down()` to the Backend trait, using raw ANSI escape sequences. Known issue: third-party backends that have not implemented these methods will fail to compile when the feature is enabled.

## Relevance to ion

ion already uses direct crossterm (no ratatui) with `insert_before` for pushing chat history to scrollback. Key takeaways:

1. **Dynamic viewport height is essential** -- Ratatui cannot do this natively. Codex solved it with a derived Terminal + forked `set_viewport_area`. ion avoids this entirely by using direct crossterm.
2. **Scrolling regions prevent flicker** -- The `\x1b[X;Yr` ANSI sequence + scroll is what makes `insert_before` smooth. ion could adopt this pattern directly with crossterm.
3. **Full viewport ownership is a trap** -- Codex learned the hard way that owning scrollback, selection, and copy across all terminal environments is extremely difficult. Their hybrid model (inline viewport + append-only scrollback) is the pragmatic choice.
4. **The fork is minimal** -- One public setter + one dependency bump. The real work is in the custom Terminal wrapper. This validates ion's approach of using crossterm directly rather than fighting ratatui's abstractions.
5. **Multiplexer compatibility matters** -- Zellij truncation issues, tmux scrollback issues, and alt-screen behavior differences are real-world problems that affect users.

## References

- [Codex TUI source](https://github.com/openai/codex/tree/main/codex-rs/tui/src)
- [custom_terminal.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/custom_terminal.rs)
- [nornagon/ratatui fork](https://github.com/nornagon/ratatui/tree/nornagon-v0.29.0-patch)
- [PR #7601 - Rework TUI viewport, history printing, selection/copy](https://gitmemories.com/openai/codex/issues/7601)
- [PR #9640 - Remove TUI2](https://github.com/openai/codex/pull/9640)
- [Issue #8344 - Don't mess with the native TUI](https://github.com/openai/codex/issues/8344)
- [Issue #6456 - Zellij scrollback truncation](https://github.com/openai/codex/issues/6456)
- [Issue #1064 - Scroll navigation](https://github.com/openai/codex/issues/1064)
- [Discussion #2503 - Scrolling through conversation history](https://github.com/openai/codex/discussions/2503)
- [ratatui PR #1341 - scrolling-regions feature](https://github.com/ratatui/ratatui/pull/1341)
- [ratatui Issue #584 - Inline viewport flickering](https://github.com/ratatui/ratatui/issues/584)
- [ratatui Issue #984 - Dynamic inline viewport height](https://github.com/ratatui/ratatui/issues/984)
- [ratatui Issue #1426 - insert_lines_before proposal](https://github.com/ratatui/ratatui/issues/1426)
- [ratatui Issue #2086 - Inline viewport resize](https://github.com/ratatui/ratatui/issues/2086)
- [ratatui Issue #625 - ScrollbarState limitations](https://github.com/ratatui/ratatui/issues/625)
- [DeepWiki Codex TUI analysis](https://deepwiki.com/openai/codex/3.2-tui-implementation)
