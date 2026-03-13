# Inline TUI Patterns: Scrollback-Preserving Terminal Applications

Research on terminal applications that print to native scrollback while maintaining a dynamic input area.

**Date:** 2026-01-26

## Executive Summary

The "inline viewport with dynamic input" pattern is solved differently across ecosystems:

| Category          | Best Example              | Technique                           | Scrollback      | Multi-line   |
| ----------------- | ------------------------- | ----------------------------------- | --------------- | ------------ |
| Python REPLs      | prompt_toolkit            | Cursor repositioning + clear-to-end | Yes             | Yes          |
| Rust Line Editors | reedline, rustyline-async | Crossterm escape sequences          | Yes             | Yes          |
| IRC Clients       | tiny (Rust)               | Custom termbox, double-buffering    | No (fullscreen) | Limited      |
| Database CLIs     | pgcli/mycli/litecli       | prompt_toolkit                      | Yes             | Yes          |
| Node.js           | terminal-kit              | ScreenBuffer with delta rendering   | Configurable    | Yes          |
| Python TUI        | Textual inline mode       | ANSI escapes + clear-to-end         | Yes             | Yes          |
| AI Agents         | Claude Code (Ink)         | Cursor-up + full redraw             | Yes             | Yes (issues) |

**Key insight:** The cleanest implementations (prompt_toolkit, reedline) treat the input area as a "transient zone" that can be erased and redrawn, while completed output is pushed above and becomes permanent scrollback.

## Pattern 1: Readline-Style (Transient Input Zone)

Used by: **prompt_toolkit**, **reedline**, **rustyline-async**, **pgcli/mycli/litecli**

### How It Works

1. Output prints normally to terminal (becomes scrollback)
2. Input prompt rendered at cursor position
3. As user types, input area grows/shrinks
4. On submit: input becomes scrollback, new prompt appears below

### prompt_toolkit (Python)

The foundation for pgcli, mycli, litecli, ptpython, and IPython.

**Key features:**

- Multi-line editing (press Enter after `:` enters multi-line mode, Alt+Enter to submit)
- Syntax highlighting while typing
- Auto-suggestions (like fish shell)
- Vi and Emacs keybindings
- Mouse support for cursor positioning

**Rendering approach:**

- Uses a `Renderer` that tracks what's on screen
- Moves cursor up to overwrite previous frame
- Clears from cursor to end of screen for shrinking
- Non-fullscreen mode: "consumes the least amount of space required for the layout"

**Multi-line input:**

```python
# ptpython: Enter after colon enters multi-line mode
# Alt+Enter or Esc+Enter to execute
>>> def foo():
...     return 42
```

**Limitation:** Cannot modify scrollback above the visible viewport - must do full clear/redraw if first changed line is above viewport.

**Source:** [prompt_toolkit docs](https://python-prompt-toolkit.readthedocs.io/en/master/)

### reedline (Rust - Nushell)

Nushell's line editor, built on crossterm.

**Architecture:**

- `Painter` struct handles terminal output
- `LineBuffer` holds in-memory representation with cursor position
- `Highlighter` trait produces `StyledText` for syntax highlighting
- Supports Kitty keyboard protocol for enhanced key events

**Multi-line support:**

- Enter after open bracket `[`, `{`, `(` continues to next line
- Enter after trailing pipe `|` continues
- Validator determines line completeness

**Source:** [reedline docs](https://docs.rs/reedline/latest/reedline/), [Nushell line editor](https://www.nushell.sh/book/line_editor.html)

### rustyline-async (Rust)

Solves the "concurrent output while editing" problem.

**Key feature:**

> "Lines written to the associated `SharedWriter` while `readline()` is in progress will be output to the screen above the input line."

**Architecture:**

- `Readline` handles input with history
- `SharedWriter` (clonable) for async output
- Uses crossterm for escape sequences
- Built-in grapheme cluster support

**Use case:** Perfect for chat-like applications where messages arrive while user is typing.

**Source:** [rustyline-async docs](https://docs.rs/rustyline-async/latest/rustyline_async/)

## Pattern 2: IRC Client Style (Split Window)

Used by: **weechat**, **irssi**, **tiny**

### How It Works

1. Screen divided into regions (typically via scroll regions or fullscreen)
2. Message history in upper region (scrollable)
3. Input bar fixed at bottom
4. Status/channel list in side panels

### weechat/irssi

Traditional IRC clients using ncurses.

**Scrollback management:**

- `scrollback_lines` controls max lines kept
- `scrollback_burst_remove` optimizes memory management
- Page Up/Down navigates history
- Buffer = total text, Window = visible portion

**Input handling:**

- Single-line input bar at bottom
- `history_max_lines` for input recall
- Arrow keys for navigation, Ctrl+Arrow for word movement

**Limitation:** Typically fullscreen (alternate screen buffer), so no native terminal scrollback.

**Source:** [WeeChat docs](https://weechat.org/files/doc/stable/weechat_faq.en.html), [irssi docs](https://irssi.org/documentation/)

### tiny (Rust IRC Client)

Custom TUI with double-buffering.

**Architecture:**

- Uses `termbox_simple` for terminal manipulation
- Grid-based internal representation (character + colors per cell)
- Double-buffering: back buffer (pending) vs front buffer (displayed)
- Only writes changed cells to terminal

**Rendering:**

> "After manipulating the internal buffer with `change_cell`, updates are rendered with `present`."

**Input parsing:**

- Uses `term_input` library
- Hardcoded xterm escape sequences (not terminfo)

**Source:** [tiny GitHub](https://github.com/osa1/tiny), [ARCHITECTURE.md](https://github.com/osa1/tiny/blob/master/ARCHITECTURE.md)

## Pattern 3: TUI Framework Inline Mode

Used by: **Textual**, **ratatui** (Viewport::Inline)

### Textual Inline Mode (Python)

Textual added inline mode to run TUI apps without fullscreen.

**Key capabilities:**

- App appears under prompt, not occupying full height
- Dynamic height: can grow/shrink while anchored to bottom
- Uses `:inline` CSS pseudo-selector for inline-specific styling

**Rendering technique:**

1. Lines end with `\n` except final line
2. Cursor returns to start position after render
3. Smaller frames: escape code clears lines from cursor downwards
4. Mouse coordinates: query cursor position, subtract from physical coordinates

**Example:**

```python
# CSS for inline mode
Screen:inline {
    height: 10;  # Fixed 10-line height in inline mode
    border: round $accent;
}
```

**Source:** [Textual inline apps blog](https://textual.textualize.io/blog/2024/04/20/behind-the-curtain-of-inline-terminal-applications/)

### ratatui Viewport::Inline (Rust)

Native Rust inline viewport with `insert_before()` for scrollback.

**How it works:**

```rust
let mut terminal = ratatui::init_with_options(TerminalOptions {
    viewport: Viewport::Inline(8),  // 8-line viewport at cursor
});

// Push content above viewport (becomes scrollback)
terminal.insert_before(1, |buf| {
    Paragraph::new("Completed work").render(buf.area, buf);
})?;

// Draw in viewport
terminal.draw(|frame| {
    // Renders within the 8-line viewport
})?;
```

**Limitations:**

- Viewport height fixed at creation (can recreate to resize)
- Horizontal resize causes content corruption (terminal reflows lines)
- Known issue: [ratatui #2086](https://github.com/ratatui/ratatui/issues/2086)

**`scrolling-regions` feature:**

- Uses DECSTBM for flicker-free `insert_before`
- Scroll region above viewport, scroll up, draw new lines

**Source:** [ratatui inline example](https://ratatui.rs/examples/apps/inline/)

## Pattern 4: AI Agent Approaches

Used by: **Claude Code**, **OpenAI Codex**, **Gemini CLI**

### Claude Code (Ink/React)

Uses Ink with incremental rendering.

**Approach:**

- `<Static>` component marks content that becomes scrollback
- Cursor moves up to redraw dynamic content (spinners, streaming)
- No alternate screen buffer

**Known issues:**

- Full redraw on every streaming chunk (4,000-6,700 scrolls/second)
- Resize causes content loss ([#18493](https://github.com/anthropics/claude-code/issues/18493))
- Cursor position drift on Windows ([#14208](https://github.com/anthropics/claude-code/issues/14208))

**Root cause:**

> "Claude Code performs a full terminal redraw on every chunk of streaming output rather than doing incremental updates."

**Source:** [Claude Code issues](https://github.com/anthropics/claude-code/issues)

### OpenAI Codex

Attempted TUI2 redesign with sophisticated architecture (later abandoned).

**TUI2 principles:**

1. In-memory transcript is single source of truth
2. Append-only scrollback (written on suspend/exit, not during runtime)
3. Cell-based high-water mark tracks what's in scrollback
4. Display-time wrapping (content reflows on resize)

**Why abandoned:**

> "The TUI2 experiment and its related config/docs were removed, keeping Codex on the terminal-native UI"

Complexity of managing transcript, selection, scrolling, and terminal state was too high.

**Source:** [Codex TUI2 discussion](https://gitmemories.com/openai/codex/issues/7601)

## Pattern 5: Terminal Multiplexer Integration

Used by: **zellij**, **tmux**

### zellij

Rust terminal multiplexer with pane management.

**Relevant features:**

- `pane_viewport_serialization`: serialize visible viewport (not just scrollback)
- `scrollback_lines_to_serialize`: control how much scrollback to persist
- Per-pane scrollback buffers

**Alt-screen interaction issue:**

> "Because some applications enter alt-screen and don't handle mouse wheel events to scroll their internal viewport, Zellij cannot show that content in its pane scrollback."

**Suggested solution:** Config option for apps to render inline (not use alternate screen).

**Source:** [Codex/zellij issue](https://github.com/openai/codex/issues/2836)

## Pattern 6: Node.js Terminal Libraries

### terminal-kit

Full-featured terminal library with screen buffer.

**ScreenBuffer capabilities:**

- Rectangular area with cells (character + colors + blending mask)
- Delta rendering: only changed cells written
- Compositing: layer smaller buffers onto larger ones
- Scroll region support (with limitations)

**Limitation:**

> "The screenBuffer should cover the whole terminal's width, because terminals only supports full-width scrolling region."

**Alternate screen:**

```javascript
term.fullscreen({
  noAlternate: true, // Stay in main buffer, preserve scrollback
});
```

**Source:** [terminal-kit docs](https://github.com/cronvel/terminal-kit)

### Ink (React for CLI)

Used by Claude Code, Codex, Gemini CLI.

**Key feature:** `<Static>` component for non-re-rendered content.

```jsx
<Box flexDirection="column">
  {/* Becomes scrollback */}
  <Static items={completedItems}>
    {(item) => <Text key={item.id}>{item.text}</Text>}
  </Static>

  {/* Re-renders */}
  <Spinner />
  <InputArea />
</Box>
```

**Source:** [Ink GitHub](https://github.com/vadimdemedes/ink)

## Terminal Escape Sequences Reference

### Scroll Regions (DECSTBM)

```
CSI Pt ; Pb r    Set scroll region (Pt=top, Pb=bottom, 1-based)
CSI r            Reset scroll region (full screen)
```

**Behavior:**

- Scrolling only affects lines within region
- Cursor moves to (1,1) after setting region
- Content outside region is protected

### Cursor Movement

```
CSI n A          Cursor up n lines
CSI n B          Cursor down n lines
CSI row ; col H  Move cursor to position (1-based)
CSI s            Save cursor position
CSI u            Restore cursor position
```

### Clearing

```
CSI J            Erase from cursor to end of screen
CSI 1 J          Erase from start of screen to cursor
CSI 2 J          Erase entire screen
CSI K            Erase from cursor to end of line
```

### Insert/Delete Lines

```
CSI n L          Insert n lines at cursor (scroll down)
CSI n M          Delete n lines at cursor (scroll up)
```

### Synchronized Output

```
CSI ? 2026 h     Begin synchronized update
CSI ? 2026 l     End synchronized update
```

Prevents flicker by batching updates. Not universally supported.

## Recommendations for ion

### Best Approach: Hybrid Readline + Viewport

Combine rustyline-async pattern with ratatui viewport.

**Architecture:**

```
+------------------------------------------+
|  Terminal scrollback (permanent)          |
|  - Completed messages                     |
|  - Tool outputs                           |
+------------------------------------------+
|  Viewport (dynamic, managed by ratatui)   |
|  - Streaming response                     |
|  - Progress indicators                    |
|  - Input area (grows with multi-line)     |
+------------------------------------------+
```

**Implementation:**

1. **Use Viewport::Inline with dynamic height:**

   ```rust
   // Start with minimal height
   let height = calculate_input_height(input_text) + status_lines;
   // Recreate viewport when height changes significantly
   ```

2. **Push completed content to scrollback:**

   ```rust
   terminal.insert_before(message_lines, |buf| {
       render_message(buf, &message);
   })?;
   ```

3. **Handle resize gracefully:**
   - Horizontal shrink: accept visual disruption, clear and redraw
   - Vertical resize: use `set_viewport_height`

4. **Use synchronized output for flicker prevention:**
   ```rust
   write!(stderr(), "\x1b[?2026h")?;  // Begin sync
   // ... render ...
   write!(stderr(), "\x1b[?2026l")?;  // End sync
   ```

### Key Design Decisions

| Decision        | Choice                                       | Rationale                      |
| --------------- | -------------------------------------------- | ------------------------------ |
| Screen buffer   | Main (no alternate)                          | Preserve scrollback            |
| Input framework | Custom on crossterm                          | Full control, matches reedline |
| Multi-line      | Enter continues, Ctrl+Enter submits          | ptpython pattern               |
| Resize handling | Accept disruption on horizontal shrink       | Unavoidable terminal behavior  |
| Streaming       | Render in viewport until complete, then push | Prevents scrollback corruption |

### What to Avoid

1. **Full redraw on every update** - Claude Code's problem (4000+ scrolls/second)
2. **Fixed viewport height** - Must adapt to input size
3. **Fighting terminal reflow** - Accept horizontal resize disruption
4. **Complex transcript management** - Codex TUI2 abandoned this

## Sources

### Line Editors / REPLs

- [prompt_toolkit](https://python-prompt-toolkit.readthedocs.io/en/master/)
- [ptpython](https://github.com/prompt-toolkit/ptpython)
- [reedline](https://docs.rs/reedline/latest/reedline/)
- [Nushell line editor](https://www.nushell.sh/book/line_editor.html)
- [rustyline](https://github.com/kkawakam/rustyline)
- [rustyline-async](https://docs.rs/rustyline-async/latest/rustyline_async/)

### IRC Clients

- [tiny](https://github.com/osa1/tiny)
- [WeeChat](https://weechat.org/)
- [irssi](https://irssi.org/)

### Database CLIs

- [pgcli](https://www.pgcli.com/)
- [pgcli multi-line mode](https://www.pgcli.com/multi-line)
- [litecli](https://github.com/dbcli/litecli)

### TUI Frameworks

- [Textual inline mode](https://textual.textualize.io/blog/2024/04/20/behind-the-curtain-of-inline-terminal-applications/)
- [ratatui inline viewport](https://ratatui.rs/examples/apps/inline/)
- [ratatui resize issue #2086](https://github.com/ratatui/ratatui/issues/2086)
- [terminal-kit](https://github.com/cronvel/terminal-kit)
- [Ink](https://github.com/vadimdemedes/ink)

### AI Agents

- [Claude Code issues](https://github.com/anthropics/claude-code/issues)
- [Claude Code flicker analysis](https://namiru.ai/blog/claude-code-s-terminal-flickering-700-upvotes-9-months-still-broken)
- [Codex zellij issue](https://github.com/openai/codex/issues/2836)

### Terminal Escape Sequences

- [Console Virtual Terminal Sequences (Microsoft)](https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences)
- [VT100 DECSTBM](https://vt100.net/docs/vt102-ug/chapter5.html)
- [TerminalScrollRegionsDisplay](https://github.com/pdanford/TerminalScrollRegionsDisplay)
