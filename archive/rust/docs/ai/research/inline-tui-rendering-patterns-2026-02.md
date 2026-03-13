# Inline TUI Rendering Patterns: Chat History, Scrollback, and Resize (2026-02)

**Research Date**: 2026-02-13
**Purpose**: Document how leading terminal coding agents handle chat history rendering, native scrollback, and terminal resize
**Scope**: Technical rendering architecture, not feature comparison

---

## Summary Table

| Agent           | Framework                    | Rendering Model                                       | Native Scrollback                | Resize Strategy                                          | Alt-Screen       |
| --------------- | ---------------------------- | ----------------------------------------------------- | -------------------------------- | -------------------------------------------------------- | ---------------- |
| **Claude Code** | Ink (React for CLI)          | Inline w/ Static flush                                | Yes (via `<Static>`)             | Clear + re-render active area                            | No (inline only) |
| **Codex CLI**   | Ratatui (forked) + crossterm | Inline w/ DECSTBM insert                              | Yes (via `insert_history_lines`) | Configurable: auto/always/never alt-screen               | Configurable     |
| **Gemini CLI**  | Ink (React for CLI)          | Dual-mode: Inline Static or Alt-screen ScrollableList | Inline mode: yes; Alt-screen: no | Ink handles active area; alt-screen uses VirtualizedList | Configurable     |
| **OpenCode**    | OpenTUI (custom TS+Zig)      | Full alt-screen                                       | No                               | Full-screen redraw                                       | Always           |
| **Amp**         | Custom TS (Flutter-inspired) | Full alt-screen                                       | No                               | Double-buffer + present()                                | Always           |

---

## 1. Claude Code (Anthropic) -- Ink

**Source**: Closed-source, but uses `ink` (npm). Ink is open at `vadimdemedes/ink`.
**Framework**: Ink v6.x (React reconciler for terminals)

### Rendering Model

Claude Code uses Ink's **inline rendering mode** (not alternate screen). The key mechanism is the `<Static>` component combined with Ink's `log-update` system.

**Two-zone architecture**:

1. **Static zone** (above): Finalized content flushed permanently to terminal scrollback
2. **Active zone** (below): Mutable content re-rendered each frame via `eraseLines` + rewrite

### How `<Static>` Works (from `ink/src/components/Static.tsx`)

```tsx
export default function Static<T>(props: Props<T>) {
  const { items, children: render, style: customStyle } = props;
  const [index, setIndex] = useState(0);
  const itemsToRender: T[] = useMemo(() => items.slice(index), [items, index]);
  useLayoutEffect(() => {
    setIndex(items.length);
  }, [items.length]);
  // renders with internal_static flag on ink-box
  return (
    <ink-box internal_static style={style}>
      {children}
    </ink-box>
  );
}
```

The `internal_static` flag tells the renderer to separate this content. In `renderer.ts`, the renderer produces two outputs: `staticOutput` (new static content) and `output` (the active area).

### How Static Output Gets Flushed (from `ink/src/ink.tsx`)

```typescript
// In onRender():
const {output, outputHeight, staticOutput} = render(this.rootNode, ...);
const hasStaticOutput = staticOutput && staticOutput !== '\n';

if (hasStaticOutput) {
  this.log.clear();                    // erase active area
  this.options.stdout.write(staticOutput); // write static content permanently
  this.log(outputToRender);            // re-render active area below
}
```

The critical sequence:

1. `log.clear()` erases the previously rendered active area (using `ansiEscapes.eraseLines(previousLineCount)`)
2. Static output is written directly to stdout -- it becomes permanent scrollback
3. Active area is re-rendered below it

### Resize Handling (from `ink/src/ink.tsx`)

```typescript
resized = () => {
  const currentWidth = this.getTerminalWidth();
  if (currentWidth < this.lastTerminalWidth) {
    this.log.clear(); // Clear active area to prevent duplicate overlapping
    this.lastOutput = "";
    this.lastOutputToRender = "";
  }
  this.calculateLayout(); // Recalculate Yoga layout at new width
  this.onRender(); // Full re-render of active area
  this.lastTerminalWidth = currentWidth;
};
```

**Key insight**: On width decrease, Ink clears the active area first to prevent ghost artifacts from wider lines overlapping. Static content already in scrollback is NOT re-rendered -- the terminal handles rewrapping it (which may look wrong if it was hard-wrapped at a specific width).

### Known Problems

- **Scroll jumping**: Claude Code issue #826 (556 thumbsup) -- during streaming, excessive `eraseLines` + rewrite causes terminal to scroll rapidly through the full scrollback buffer
- **4000-6700 scroll events/second**: Issue #9935 -- full-screen redraw strategy overwhelms terminal multiplexers
- **`/clear` doesn't clear scrollback**: Issue #11260 -- static content persists in terminal buffer after clear
- **Resize re-renders full conversation**: Issue #17529 -- Ctrl+O toggle and resize cause disruptive scrolling

### Assessment

| Aspect                    | Rating   | Notes                                                                        |
| ------------------------- | -------- | ---------------------------------------------------------------------------- |
| Native scrollback         | Good     | Static content naturally becomes scrollback                                  |
| Resize for static content | Poor     | Terminal rewraps but Ink doesn't re-render; hard-wrapped content looks wrong |
| Resize for active area    | Moderate | Clears and redraws, but can cause flicker                                    |
| Streaming performance     | Poor     | Full active-area redraw per token chunk causes excessive scroll events       |
| Duplication on resize     | Moderate | Width-decrease clear helps but scrollback can re-appear                      |

---

## 2. Codex CLI (OpenAI) -- Ratatui + crossterm (Rust)

**Source**: `openai/codex`, `codex-rs/tui/src/`
**Framework**: Forked `ratatui::Terminal` (`custom_terminal.rs`) + crossterm, inline viewport

### Rendering Model

Codex uses a **forked ratatui Terminal** with an inline viewport (not alternate screen by default). The key innovation is `insert_history_lines` -- a function that inserts finalized content above the viewport using DECSTBM scroll regions.

**Architecture**:

1. **Viewport**: A ratatui `Buffer` pinned to the bottom N rows of the terminal
2. **History insertion**: Completed transcript cells are rendered to `ratatui::text::Line` vectors and inserted above the viewport using scroll-region manipulation
3. **Active cell**: In-flight streaming content rendered within the viewport buffer

### `insert_history_lines` Mechanism (from `insert_history.rs`)

```rust
pub fn insert_history_lines<B>(
    terminal: &mut Terminal<B>,
    lines: Vec<Line>,
) -> io::Result<()> {
    // 1. Pre-wrap lines using word-aware wrapping at current width
    let wrapped = word_wrap_lines_borrowed(&lines, area.width as usize);

    // 2. If viewport not at bottom, scroll it down to make room
    //    Uses DECSTBM (Set Scroll Region) + Reverse Index (ESC M)
    if area.bottom() < screen_size.height {
        queue!(writer, SetScrollRegion(top..screen_height))?;
        queue!(writer, MoveTo(0, area.top()))?;
        for _ in 0..scroll_amount {
            queue!(writer, Print("\x1bM"))?;  // Reverse Index
        }
        queue!(writer, ResetScrollRegion)?;
        area.y += scroll_amount;
    }

    // 3. Set scroll region to ABOVE the viewport
    //    ┌─Screen──────────────┐
    //    │┌╌Scroll region╌╌╌╌╌┐│
    //    │┆ (history goes here)┆│
    //    │█╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┘│
    //    │╭─Viewport──────────╮│
    //    ││ (active content)  ││
    //    │╰───────────────────╯│
    //    └─────────────────────┘
    queue!(writer, SetScrollRegion(1..area.top()))?;
    queue!(writer, MoveTo(0, cursor_top))?;

    // 4. Write lines with \r\n, which scrolls only within the region
    for line in wrapped {
        queue!(writer, Print("\r\n"))?;
        // ... write styled spans ...
    }

    // 5. Reset scroll region and restore cursor position
    queue!(writer, ResetScrollRegion)?;
    queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;
}
```

This is more sophisticated than Ink's approach because it:

- Uses DECSTBM to isolate the scroll region, preventing the viewport from being affected
- Pre-wraps content at the current terminal width before insertion
- Restores cursor position after insertion (cursor-position-neutral)

### Alternate Screen Modes (from `docs/tui-alternate-screen.md`)

Codex supports three modes via `config.tui.alternate_screen`:

| Mode             | Behavior                                               |
| ---------------- | ------------------------------------------------------ |
| `auto` (default) | Detects Zellij -> inline mode; elsewhere -> alt-screen |
| `always`         | Always use alt-screen (no native scrollback)           |
| `never`          | Always inline (native scrollback preserved)            |

In alt-screen mode, Codex uses a transcript pager (Ctrl+T) for reviewing history.

### Custom Terminal (from `custom_terminal.rs`)

Codex forks ratatui's `Terminal` struct to:

- Track `viewport_area` and `last_known_cursor_pos` independently
- Support double-buffering with diff-based updates (only changed cells written)
- Allow `insert_history_lines` to manipulate scroll regions without disturbing the viewport

```rust
pub struct Terminal<B> {
    backend: B,
    buffers: [Buffer; 2],        // Double buffer for diff rendering
    current: usize,
    hidden_cursor: bool,
    viewport_area: Rect,         // Where the managed viewport sits
    last_known_screen_size: Size,
    last_known_cursor_pos: Position,
}
```

### Resize Handling

```rust
pub fn resize(&mut self, screen_size: Size) -> io::Result<()> {
    self.last_known_screen_size = screen_size;
    Ok(())
}

pub fn set_viewport_area(&mut self, area: Rect) {
    self.current_buffer_mut().resize(area);
    self.previous_buffer_mut().resize(area);
    self.viewport_area = area;
}
```

On resize, the viewport area is recalculated and buffers are resized. Historical content already in scrollback is NOT re-rendered -- the terminal handles rewrapping. The viewport is redrawn at the new dimensions.

### Known Problems

- Issue #6427: "Codex Truncates Chat Messages When Scrolling" -- scrollback content can get clipped
- Issue #5576: "Output width remains truncated after resizing" -- content rendered at old width stays narrow
- Issue #355: "Resizing terminal leads to bad artifacts" -- staircase effect in prompt area
- PR #11221 fixes invalid DECSTBM ranges when viewport is at top row

### Assessment

| Aspect                | Rating   | Notes                                                        |
| --------------------- | -------- | ------------------------------------------------------------ |
| Native scrollback     | Good     | DECSTBM insertion is clean and cursor-neutral                |
| Resize for history    | Moderate | Old content stays at old width; terminal rewraps imperfectly |
| Resize for viewport   | Good     | Double-buffer diff rendering handles viewport cleanly        |
| Streaming performance | Good     | Only viewport is re-rendered, not full history               |
| Duplication on resize | Low risk | DECSTBM isolates viewport from history scroll                |

---

## 3. Gemini CLI (Google) -- Ink

**Source**: `google-gemini/gemini-cli`, `packages/cli/src/ui/`
**Framework**: Ink (React for CLI), same as Claude Code

### Rendering Model -- Dual Mode

Gemini CLI supports two rendering modes, toggled by `ui.useAlternateBuffer` in settings:

**Inline mode (default)**:
Uses Ink's `<Static>` component identically to Claude Code:

```tsx
// From MainContent.tsx
return (
  <>
    <Static
      key={uiState.historyRemountKey}
      items={[<AppHeader />, ...historyItems]}
    >
      {(item) => item}
    </Static>
    {pendingItems}
  </>
);
```

Completed history items are wrapped in `<Static>` and flushed to terminal scrollback. Pending/streaming items are rendered in the active area below.

**Alternate buffer mode**:
Uses a custom `ScrollableList` (virtualized list) within a fixed-height `Box`:

```tsx
if (isAlternateBuffer) {
  return (
    <ScrollableList
      ref={scrollableListRef}
      width={uiState.terminalWidth}
      data={virtualizedData}
      renderItem={renderItem}
      estimatedItemHeight={() => 100}
      initialScrollIndex={SCROLL_TO_ITEM_END}
    />
  );
}
```

In this mode, Gemini implements its own virtual scrolling with:

- `VirtualizedList` with estimated item heights
- Animated scrollbar (`useAnimatedScrollbar`)
- Keyboard-driven scroll (arrow keys, Page Up/Down)
- Mouse scroll support
- `overflow: "hidden"` on the root Box with `height={terminalHeight}`

### Resize Handling

- **Inline mode**: Same as Claude Code (Ink handles it: clear active area on width decrease, re-layout, re-render)
- **Alt-buffer mode**: Ink re-renders the full `ScrollableList` at new dimensions; `VirtualizedList` recalculates visible items

### Known Problems

- Issue #13271: "Terminal's Native Scrollbar Becomes Unusable in Long Chat Sessions" -- inline mode scrollbar fails
- Issue #17289: "Terminal rendering corruption during resize when using alternate buffer mode"
- Issue #2623: Comprehensive scrolling/resize meta-issue with many sub-issues (content repetition on resize, jumpy terminal, logo spam)
- Issue #7022: "Use overflow:scroll for all cases" -- standardizing scroll approach

### Key Feature: Constrained Height

Gemini limits response display height even in inline mode:

```tsx
availableTerminalHeight={
  (uiState.constrainHeight && !isAlternateBuffer) || isAlternateBuffer
    ? availableTerminalHeight : undefined
}
```

This prevents extremely long responses from pushing the active area too far, mitigating scroll issues.

### Assessment

| Aspect                | Rating                     | Notes                                              |
| --------------------- | -------------------------- | -------------------------------------------------- |
| Native scrollback     | Good (inline) / None (alt) | Dual mode gives user choice                        |
| Resize for history    | Poor (inline)              | Same Ink limitations as Claude Code                |
| Resize for viewport   | Moderate                   | Alt-mode virtualizes but still has corruption bugs |
| Streaming performance | Moderate                   | Inherits Ink's full-active-area redraw             |
| Duplication on resize | Moderate                   | Resize in alt-mode has known corruption issues     |

---

## 4. OpenCode (anomalyco/SST) -- OpenTUI

**Source**: `anomalyco/opencode`, `anomalyco/opentui`
**Framework**: OpenTUI (TypeScript core + Zig rendering backend)

### Rendering Model

OpenCode uses **full alternate-screen mode** exclusively via OpenTUI. OpenTUI is a purpose-built TUI framework with:

- Yoga-driven Flexbox layout (same layout engine as Ink)
- Zig-based rendering backend for performance
- React or SolidJS reconciler
- Full-screen control with custom viewport/scrollbox primitives

### Architecture

OpenTUI uses a compositing model:

1. Layout tree computed via Yoga
2. Zig backend renders to a double-buffer
3. Diff-based terminal updates (only changed cells written)
4. Built-in scrollbox with overflow:scroll

Scrollback is handled entirely within the application -- the terminal's native scrollback is not used.

### Resize Handling

On resize, OpenTUI recalculates the full Yoga layout tree and redraws. Since it owns the entire screen, this is straightforward but means:

- All content must be retained in application memory
- Terminal native search does not work
- Scroll position is managed internally

### Known Problems

- Issue #3020: "Scrollback in Zellij Pane Broken" -- no native scrollback by design
- Issue #106: "Requests option to not use alternate (fullscreen) screen mode"
- Issue #10391: "TUI layout malformed when height is half screen"
- Rendering glitches accumulate over time; resize triggers force-redraw fix

### Assessment

| Aspect                | Rating | Notes                                             |
| --------------------- | ------ | ------------------------------------------------- |
| Native scrollback     | None   | Full alt-screen, app-managed scroll               |
| Resize                | Good   | Full redraw at new dimensions, no ghost artifacts |
| Streaming performance | Good   | Zig backend, diff updates                         |
| Duplication on resize | None   | Full redraw eliminates duplicates                 |
| Terminal search       | None   | Cannot use Ctrl+Shift+F or terminal find          |

---

## 5. Amp (Sourcegraph) -- Custom TS Framework

**Source**: Closed-source CLI (npm `@sourcegraph/amp`). Architecture described in blog post.
**Framework**: Custom TypeScript TUI framework, ported from Zig, Flutter-inspired

### Rendering Model

From Sourcegraph's blog post "A Codebase by an Agent for an Agent":

> "The Amp TUI framework uses a **double-buffering** approach toward updating the screen. We keep a front and a back screen -- one for the current state, one for the updates, and swap the screens."

The function that swaps screens is called `present()`. The framework is Flutter-inspired with:

- Widget tree rendering
- Animation subsystem
- Click handlers and keyboard shortcuts
- Custom scrollbar implementation for modals

Amp uses **full alternate-screen mode**. Thread-based navigation (j/k, Space to expand) manages history within the app.

### Assessment

| Aspect            | Rating          | Notes                                    |
| ----------------- | --------------- | ---------------------------------------- |
| Native scrollback | None            | Full alt-screen                          |
| Resize            | Good (presumed) | Double-buffer swap handles clean redraws |
| Terminal search   | None            | App-managed                              |

---

## 6. pi-mono / Pi (badlogic) -- Custom TS TUI

**Source**: `badlogic/pi-mono` (not accessible; architecture from prior research)
**Framework**: Custom TypeScript TUI `@mariozechner/pi-tui`

### Rendering Model

Pi uses a custom differential rendering system:

- Components implement `render(width: number): string[]` returning one string per line
- `invalidate()` clears cached render state
- Ctrl+O toggles tool output expansion
- Full-screen mode with custom scrolling

Likely uses alternate screen based on the component interface design (fixed viewport, no Static-like mechanism).

---

## Key Technical Patterns

### Pattern 1: Ink's Static Flush (Claude Code, Gemini CLI inline)

```
Render loop:
  1. React reconciler processes state changes
  2. Yoga computes layout
  3. renderer.ts separates Static content from active content
  4. If new Static content exists:
     a. log.clear() -- erase active area (eraseLines)
     b. stdout.write(staticOutput) -- permanent scrollback
     c. log(activeOutput) -- re-render active area
  5. Else: throttledLog(activeOutput) -- update active area in-place
```

**Strength**: Simple model, native scrollback works
**Weakness**: Static content is write-once; cannot be updated or reformatted on resize. Active area redraw is expensive during streaming.

### Pattern 2: DECSTBM Scroll Region Insertion (Codex CLI Rust)

```
insert_history_lines:
  1. Pre-wrap lines at current terminal width
  2. If viewport not at bottom, scroll it down via DECSTBM + Reverse Index
  3. Set scroll region to rows ABOVE viewport
  4. Write lines within the scroll region (terminal scrolls only within region)
  5. Reset scroll region, restore cursor
```

**Strength**: Viewport is undisturbed during history insertion; pre-wrapping ensures consistent appearance
**Weakness**: Pre-wrapped content is baked at insertion width; terminal rewrap on resize may look different. Complex ANSI escape sequences with edge cases (top-row viewport, PR #11221).

### Pattern 3: Full Alt-Screen with Virtual Scroll (OpenCode, Amp)

```
Render loop:
  1. Compute full layout tree (Yoga/custom)
  2. Render to back buffer
  3. Diff against front buffer
  4. Write only changed cells to terminal
  5. Swap buffers (present())
```

**Strength**: No scrollback artifacts, clean resize, consistent rendering
**Weakness**: No native terminal scrollback or search; all history must be retained in app memory; higher memory usage for long sessions.

---

## Resize: The Universal Hard Problem

Every agent struggles with resize. The fundamental tension:

| Approach                   | Resize Behavior                     | Problem                                                              |
| -------------------------- | ----------------------------------- | -------------------------------------------------------------------- |
| Native scrollback (inline) | Terminal rewraps old lines          | Content pre-formatted at old width looks wrong; can't be re-rendered |
| Alt-screen (full)          | App redraws everything              | No native scrollback; long sessions need internal scroll management  |
| Hybrid (Codex)             | Old content stays; viewport redraws | Width mismatch between old and new content                           |

**No agent has solved the "re-render historical scrollback on resize" problem.** The terminal owns the scrollback buffer and provides no API to rewrite it. The only option would be `\033[3J` (clear scrollback) + full re-emission, which destroys scroll position.

### Boris Cherny's Analysis (Threads, Dec 2025)

The Codex team member describes the two regions:

> "When rendering in a terminal there are two regions: the viewport at the bottom and the scrollback buffer above it. When content exceeds the viewport height, the top row gets pushed into scrollback and some of the rendering happens offscreen."

This is the fundamental constraint that all inline-mode agents work within.

---

## Recommendations for ion

### Current ion Architecture

ion uses direct crossterm with:

- Chat history printed to stdout via `insert_before` (conceptually similar to Codex's approach)
- Bottom UI via direct cursor positioning at `terminal_height - ui_height`
- Custom input composer with ropey buffer

### What to Adopt

1. **Keep the inline + insert-before model**. It is the same fundamental approach as Codex CLI (the most sophisticated inline implementation) and Claude Code / Gemini CLI's Static flush. Native scrollback is a major usability advantage that alt-screen agents lack.

2. **Study Codex's DECSTBM approach in detail**. Their `insert_history_lines` is the gold standard for inserting content above a viewport without disturbing it. Key techniques:
   - Pre-wrap at current width before insertion
   - Use DECSTBM to isolate scroll regions
   - Make insertion cursor-position-neutral
   - Handle edge cases: viewport at top row, viewport at bottom of screen

3. **Accept that historical scrollback will not re-render on resize**. No agent does this. The terminal owns that content. Focus instead on:
   - Making new content render correctly at the new width
   - Ensuring the viewport/bottom-UI redraws cleanly
   - Not duplicating history on resize (the primary bug)

4. **Consider Gemini's constrained-height approach** for streaming content. Limiting the active area height prevents extremely long responses from pushing the viewport too far down, which causes the scroll-jumping issues that plague Claude Code.

5. **Do not adopt alt-screen as default**. The trend is toward inline: Codex added `auto`/`never` modes specifically because users demanded native scrollback. OpenCode has a top-requested issue (#106) for non-alt-screen mode.

### What to Avoid

1. **Full-active-area redraw per token** (Ink's approach). This causes the 4000+ scroll events/second problem. ion's current append-delta approach is better.

2. **Hard-wrapping content at render time** without preserving canonical form. Always keep the unwrapped canonical text and wrap on output. This makes resize-time rewrap possible for the viewport.

3. **Rewriting scrollback on resize**. It is tempting but impossible without `\033[3J` which destroys scroll position and history.

### Architecture Alignment

ion's two-plane model from `chat-softwrap-scrollback-2026-02.md` is well-aligned with the industry:

| ion Design                             | Industry Equivalent                                     |
| -------------------------------------- | ------------------------------------------------------- |
| Plane A: append-only chat history      | Codex's `insert_history_lines` / Ink's `<Static>` flush |
| Plane B: ephemeral bottom UI           | Codex's viewport / Ink's active area                    |
| Canonical transcript + viewport rewrap | Codex pre-wraps on insert; Ink re-layouts               |
| No scrollback rewrite                  | Universal across all agents                             |

The main gap is that ion needs robust DECSTBM-based insertion (like Codex) rather than raw stdout writes, to prevent viewport disruption during history insertion.

---

## Source Code References

| Agent                      | Key Files                                                                                                                                                                                                           | URL                                                 |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| Ink                        | `src/components/Static.tsx`, `src/ink.tsx`, `src/renderer.ts`, `src/log-update.ts`                                                                                                                                  | github.com/vadimdemedes/ink                         |
| Codex CLI (Rust)           | `codex-rs/tui/src/insert_history.rs`, `codex-rs/tui/src/custom_terminal.rs`, `codex-rs/tui/src/chatwidget.rs`, `codex-rs/tui/src/history_cell.rs`                                                                   | github.com/openai/codex                             |
| Codex docs                 | `docs/tui-alternate-screen.md`                                                                                                                                                                                      | github.com/openai/codex                             |
| Gemini CLI                 | `packages/cli/src/ui/components/MainContent.tsx`, `packages/cli/src/ui/layouts/DefaultAppLayout.tsx`, `packages/cli/src/ui/hooks/useAlternateBuffer.ts`, `packages/cli/src/ui/components/shared/ScrollableList.tsx` | github.com/google-gemini/gemini-cli                 |
| OpenTUI                    | Core library                                                                                                                                                                                                        | github.com/anomalyco/opentui                        |
| Amp blog                   | "A Codebase by an Agent for an Agent"                                                                                                                                                                               | ampcode.com/notes/by-an-agent-for-an-agent          |
| Codex PR #11221            | Scroll-region bounds hardening                                                                                                                                                                                      | github.com/openai/codex/pull/11221                  |
| BubbleTea discussion #1482 | Scrollback buffer patterns                                                                                                                                                                                          | github.com/charmbracelet/bubbletea/discussions/1482 |
