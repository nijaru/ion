# Ion TUI Architecture

**Date:** 2026-02-22
**Status:** COMPLETE — all 8 steps implemented and committed.
**Reference:** See `ai/design/tui-lib-spec.md` for the general-purpose library vision (Claude Desktop conversation). This document was ion-specific cleanup, not the full library.

> **Note:** This was an internal cleanup of ion's existing TUI. It is NOT the `crates/tui/` general-purpose library described in `tui-lib-spec.md`. That library does not exist yet and needs to be built from scratch.

---

## 1. The Hybrid Rendering Model

Ion's TUI has two fundamentally different rendering regions that must be handled differently:

| Region       | Model                                               | Who owns it       |
| ------------ | --------------------------------------------------- | ----------------- |
| Chat history | Printed to terminal scrollback; never touched again | Terminal (native) |
| Bottom UI    | Cursor-positioned, redrawn every frame              | Ion               |

This is **not** a full-screen buffer model and should not become one. The scrollback model is intentional — it gives ion infinite history, native terminal scroll, and no memory cost for old messages. The general-purpose lib spec's `Buffer + diff` approach applies only to the bottom UI.

The current code gets this right. The problem is how each region is rendered, not the split itself.

---

## 2. Current Problems

### 2.1 rnk as ANSI generator (wrong use of the library)

rnk is a layout library. Ion uses it only to apply ANSI escape codes to styled text strings. Every call looks like:

```rust
let element = RnkBox::new()
    .flex_direction(FlexDirection::Row)
    .width(width)
    .child(text.wrap(wrap).into_element())
    .into_element();
let rendered = rnk::render_to_string(&element, width);
rendered.lines().next().unwrap_or_default().to_string()
```

This runs rnk's full flexbox layout + render pipeline to get back one ANSI-escaped line. The `.lines().next()` discards all multi-line output. The `width` passed to the container is `display_width(content)` — not the terminal width — specifically to prevent rnk from padding to container width (a workaround for rnk's behavior, fixed by an earlier bug where `render_to_string_no_trim` was used instead).

**Fix:** Replace rnk with a thin ANSI string builder. We only use: color (fg/bg), bold, dim, italic, underline, strikethrough, inverse. That's 20 lines of crossterm escape code, not a layout engine.

### 2.2 `saturating_sub(1)` scattered everywhere

The "reserve one cell to prevent terminal autowrap at the right edge" pattern appears in 5+ places:

```rust
let max_cells = width.saturating_sub(1) as usize;  // bottom_ui.rs
let max_cells = width.saturating_sub(1) as usize;  // terminal.rs
let max_cells = width.saturating_sub(1) as usize;  // rnk_text.rs
```

**Fix:** One terminal constant or method. `fn safe_width(terminal_width: u16) -> usize`.

### 2.3 Two word-wrap implementations

`chat_renderer.rs` has a custom `wrap_line()` using `UnicodeWidthChar` directly. rnk has its own internal wrapping. They can disagree on CJK boundary cases. `calculate_input_height` uses `visual_line_count` from `ComposerState`. The composer rendering also uses `visual_line_count`. These are separate call sites.

**Fix:** One `wrap_text(text: &str, width: usize) -> Vec<String>` function used everywhere — chat renderer, composer height calculation, and composer rendering.

### 2.4 `ComposerState.last_width` implicit dependency

`last_width` is set during render and used for cursor calculations. On resize, the width changes before the next render, making cursor math wrong for one frame. This causes brief visual glitches on resize.

**Fix:** Pass width explicitly to all composer methods. No cached width in state. `ComposerState` methods take `width: usize` as a parameter; the render call provides it from the terminal.

### 2.5 No test infrastructure

`StyledLine` has no `to_plain_string()` method. `ChatRenderer::build_lines()` returns `Vec<StyledLine>` but you can't assert on content:

```rust
// Can write:
assert_eq!(lines.len(), 3);
// Cannot write:
assert_eq!(lines[0].plain_text(), "› Hello world");
```

The composer test suite (`composer/tests.rs`) tests buffer mutations but not rendered output.

**Fix:** `StyledLine::plain_text() -> String` and `StyledLine::ansi_string(width: usize) -> String`. Enables:

```rust
let lines = ChatRenderer::build_lines(&entries, None, 80);
assert_eq!(lines[0].plain_text(), "› Hello world");
assert_eq!(lines.len(), 2); // wraps at 80
```

### 2.6 `ChatRenderer::build_lines` does too much

~400 lines handling: markdown parsing, code block detection, thinking block formatting, tool call display, line wrapping, span construction. One function, impossible to test individual paths.

**Fix:** Split into testable units:

```rust
fn render_user_message(text: &str, width: usize) -> Vec<StyledLine>
fn render_agent_text(markdown: &str, width: usize) -> Vec<StyledLine>
fn render_tool_call(name: &str, args: &Value, width: usize) -> Vec<StyledLine>
fn render_tool_result(output: &str, width: usize) -> Vec<StyledLine>
fn render_thinking_block(text: &str, width: usize) -> Vec<StyledLine>
```

Each pure function, each independently testable.

### 2.7 `BeginSynchronizedUpdate` inconsistency

Synchronized update (flicker prevention) is called from `run.rs` but not wrapped consistently around every render path. The selector renderer and OAuth confirm dialog render outside the synchronized block in some code paths.

**Fix:** The terminal layer wraps every render call in synchronized update automatically.

---

## 3. Target Architecture

### 3.1 Layers

```
┌─────────────────────────────────────────────────────┐
│  run.rs — event loop, frame pipeline                │
│  (PreOp, ChatInsert, FramePrep — keep as-is)        │
├──────────────────┬──────────────────────────────────┤
│  chat/           │  bottom_ui/                      │
│  Scrollback      │  Buffer (bottom UI only)         │
│  rendering       │  + diff + cursor positioning     │
├──────────────────┴──────────────────────────────────┤
│  StyledLine / StyledSpan / Style                    │
│  (ion's canonical styled text types)                │
├─────────────────────────────────────────────────────┤
│  text/                                              │
│  wrap_text, display_width, grapheme ops             │
│  (one implementation, used everywhere)              │
├─────────────────────────────────────────────────────┤
│  ansi/                                              │
│  Thin ANSI escape builder (replaces rnk)            │
│  apply_style(s: &str, style: Style) -> String       │
├─────────────────────────────────────────────────────┤
│  crossterm — raw mode, events, cursor               │
└─────────────────────────────────────────────────────┘
```

### 3.2 The `ansi` module (replaces rnk)

**Use crossterm's existing styled content API — do not write raw escape codes.**

crossterm 0.29 ships `ContentStyle` + `StyledContent<D: Display>`. `StyledContent<D>` implements `Display` by calling `PrintStyledContent`, which writes `SetForegroundColor` / `SetBackgroundColor` / `SetAttributes` / content / reset — all correctly. This is already in our dependency tree.

```rust
use crossterm::style::{Attribute, Attributes, ContentStyle, Color as CtColor};

/// Map our TextStyle to a crossterm ContentStyle.
fn to_content_style(style: &TextStyle) -> ContentStyle {
    let mut cs = ContentStyle::default();
    if let Some(fg) = style.foreground_color {
        cs.foreground_color = Some(map_color(fg));
    }
    if let Some(bg) = style.background_color {
        cs.background_color = Some(map_color(bg));
    }
    let mut attrs = Attributes::default();
    if style.bold        { attrs.set(Attribute::Bold); }
    if style.dim         { attrs.set(Attribute::Dim); }
    if style.italic      { attrs.set(Attribute::Italic); }
    if style.underlined  { attrs.set(Attribute::Underlined); }
    if style.crossed_out { attrs.set(Attribute::CrossedOut); }
    if style.reverse     { attrs.set(Attribute::Reverse); }
    cs.attributes = attrs;
    cs
}

/// Apply style to a string. Returns ANSI-escaped string.
/// Uses crossterm's Display impl — no manual escape code tables.
pub(crate) fn apply_style(s: &str, style: &TextStyle) -> String {
    format!("{}", to_content_style(style).apply(s))
}

/// Map our Color enum to crossterm's Color.
/// This replaces the private map_color in terminal.rs.
fn map_color(color: Color) -> CtColor { ... }
```

The `Color` and `TextStyle` types stay in `terminal.rs` (they are ion's interface layer). The `ansi` module is the only place that maps them to crossterm and emits escape codes. `map_color` moves from `terminal.rs` to `ansi.rs`; the `to_rnk_span` function and rnk imports are deleted.

### 3.3 The `text` module (single source of truth for width/wrap)

```rust
/// Display width of a string in terminal cells.
/// Handles CJK wide chars (width=2) and zero-width combining chars.
pub fn display_width(s: &str) -> usize

/// Grapheme-aware truncation to `max_cells` display cells.
/// Never splits a wide char — pads with one space when a wide char would overflow.
/// Returns String (not &str) because the padding case requires allocation.
pub fn truncate_to_width(s: &str, max_cells: usize) -> String

/// Word-wrap a string to `width` display cells.
/// Returns owned strings (each line owns its content).
/// Word boundary: whitespace-delimited. No regex.
pub fn wrap_text(s: &str, width: usize) -> Vec<String>

/// Safe terminal width: `terminal_width - 1` to prevent autowrap.
pub fn safe_width(terminal_width: u16) -> usize {
    terminal_width.saturating_sub(1) as usize
}
```

### 3.4 `StyledSpan` and `StyledLine` (unchanged types, new methods)

**Step 2 adds only `plain_text()` and `display_width()`.** These are pure additions with no rnk dependency, which immediately unblock testing. `to_ansi()` requires Step 5 (the `ansi` module) and is deferred.

**Step 2** adds to `StyledSpan` and `StyledLine`:

```rust
impl StyledSpan {
    pub fn plain_text(&self) -> &str
}

impl StyledLine {
    pub fn plain_text(&self) -> String
    pub fn display_width(&self) -> usize
}
```

**Step 5** reimplements the existing write methods using the `ansi` module and adds `to_ansi`:

```rust
impl StyledLine {
    pub fn to_ansi(&self, width: usize) -> String
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()>
    pub fn write_to_width<W: Write>(&self, w: &mut W, width: u16) -> io::Result<()>
}
```

`to_ansi` walks spans, truncates each span's content to fit remaining width using `text::truncate_to_width`, then applies styling via `ansi::apply_style`. The rnk-based `write_to` / `write_to_width` are replaced with span-walking + `ansi::apply_style` calls.

Testing becomes straightforward:

```rust
#[test]
fn user_message_has_prefix() {
    let entries = vec![user_entry("hello world")];
    let lines = ChatRenderer::build_lines(&entries, None, 80);
    assert_eq!(lines[0].plain_text(), "› hello world");
}

#[test]
fn long_message_wraps() {
    let entries = vec![user_entry("word ".repeat(20))];
    let lines = ChatRenderer::build_lines(&entries, None, 40);
    assert!(lines.len() > 1);
    assert!(lines[0].display_width() <= 40);
}
```

### 3.5 Bottom UI: Buffer-backed rendering

The bottom UI (progress bar, input box, status line) should use a `Buffer` for two reasons:

1. Enables `render_to_lines()` for snapshot testing
2. Diff-based flush eliminates the current clear-and-redraw approach (reduces flicker)

```rust
/// A 2D grid of styled cells for the bottom UI region.
pub struct Buffer {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
}

pub struct Cell {
    pub symbol: String,   // grapheme cluster, usually 1 char
    pub style: TextStyle,
    pub wide: bool,       // true for second cell of a wide char
}

impl Buffer {
    pub fn new(width: u16, height: u16) -> Self
    pub fn set_string(&mut self, x: u16, y: u16, s: &str, style: TextStyle)
    pub fn set_styled_line(&mut self, y: u16, line: &StyledLine)

    /// For testing: lines of plain text
    pub fn to_plain_lines(&self) -> Vec<String>
    /// For testing: lines with ANSI escapes
    pub fn to_ansi_lines(&self) -> Vec<String>

    /// Minimal draw commands to transform `prev` into `self`
    pub fn diff(&self, prev: &Buffer) -> Vec<DrawCommand>
}

/// Write diff commands to terminal
pub fn flush_commands<W: Write>(w: &mut W, commands: Vec<DrawCommand>) -> io::Result<()>
```

Snapshot tests for the bottom UI:

```rust
#[test]
fn status_bar_shows_model() {
    let buf = render_status_bar(width: 80, model: "claude-opus-4-6", tokens: 1234);
    let line = buf.to_plain_lines()[0];
    assert!(line.contains("claude-opus-4-6"));
    assert!(line.contains("1234"));
}
```

### 3.6 Composer: explicit width, no cached state

New/changed signatures after `last_width` removal:

```rust
impl ComposerState {
    pub fn cursor_pos(&self, buffer: &ComposerBuffer, width: usize) -> (u16, u16)
    pub fn visual_height(&self, buffer: &ComposerBuffer, width: usize) -> usize
    pub fn render_lines(&self, buffer: &ComposerBuffer, width: usize) -> Vec<StyledLine>
    pub fn move_up(&mut self, buffer: &ComposerBuffer, width: usize) -> bool
    pub fn move_down(&mut self, buffer: &ComposerBuffer, width: usize) -> bool
    pub fn move_to_visual_line_start(&mut self, buffer: &ComposerBuffer, width: usize)
    pub fn move_to_visual_line_end(&mut self, buffer: &ComposerBuffer, width: usize)
    // invalidate_width() deleted
}
```

`calculate_input_height` in `layout.rs` calls `visual_height` with the current width. The renderer calls `render_lines` with the same width. They cannot disagree because they use the same function.

Remove `last_width: usize` from `ComposerState`. Callers of `move_up` / `move_down` / visual-line navigation methods already have the current terminal width from the event loop and pass it explicitly. `invalidate_width()` is deleted — there is no cached state to invalidate.

---

## 4. The Composer (Ion-Specific, Not From the Lib Spec)

The lib spec's `InputState` uses `Vec<String>`. Ion's composer requires:

- **ropey `Rope`** — O(log n) insert/delete, handles large pastes efficiently
- **Blob system** — large pastes become `«Pasted #N»` placeholders resolved at submit time
- **Kill ring** — Ctrl+K / Ctrl+Y
- **Visual line wrapping** — multi-line display of single logical lines
- **Attachment placeholders** — `@path` references rendered inline

Ion's composer is a `Canvas`-equivalent: custom rendering logic, not the lib's `Input` widget. It stays in `src/tui/composer/` and is not part of any extracted library.

```rust
/// Ion's composer owns both the buffer and its render state.
pub struct Composer {
    pub buffer: ComposerBuffer,   // ropey-backed, blob storage
    pub state: ComposerState,     // cursor, scroll, preferred col
}

impl Composer {
    /// Render to visual lines at given width.
    pub fn render(&self, width: usize) -> Vec<StyledLine>

    /// Height in visual lines at given width (same computation as render).
    pub fn visual_height(&self, width: usize) -> usize

    /// Cursor position for terminal cursor placement.
    pub fn cursor_pos(&self, width: usize) -> (u16, u16)
}
```

---

## 5. Chat Rendering: Scrollback Model (Unchanged)

Chat history is printed to stdout and owned by the terminal's scrollback. This is intentional and correct for a coding agent. Ion adds one abstraction:

```rust
/// A rendered message ready for scrollback.
pub struct RenderedMessage {
    pub lines: Vec<StyledLine>,
}

impl RenderedMessage {
    /// Write all lines to scrollback.
    pub fn write_to_scrollback<W: Write>(&self, w: &mut W, width: u16) -> io::Result<()>
}
```

The `FramePrep` / `PreOp` / `ChatInsert` pipeline in `run.rs` stays as-is — it's well-designed and handles the complexity of the scrollback/tracking/overflow transitions correctly.

---

## 6. Chat Renderer: Testable Split

`ChatRenderer::build_lines` splits into:

```rust
pub mod chat_renderer {
    pub fn render_user_message(text: &str, width: usize) -> Vec<StyledLine>
    pub fn render_agent_text(markdown: &str, width: usize) -> Vec<StyledLine>
    pub fn render_tool_call(name: &str, args: &Value, width: usize) -> Vec<StyledLine>
    pub fn render_tool_result(output: &str, width: usize) -> Vec<StyledLine>
    pub fn render_thinking(text: &str, width: usize) -> Vec<StyledLine>

    /// Orchestrates the above for a full message list.
    pub fn build_lines(
        entries: &[MessageEntry],
        queued: Option<&Vec<String>>,
        wrap_width: usize,
    ) -> Vec<StyledLine>
}
```

All are pure functions of their inputs. All testable without a terminal.

---

## 7. What This Is Not

- Not a full-screen buffer model — chat history stays in scrollback
- Not a general-purpose TUI library — see `tui-lib-spec.md` for that vision
- Not a rewrite of the frame pipeline (`PreOp`/`ChatInsert`/`FramePrep`) — it works

The lib spec (Taffy layout, App trait, Element tree, fullscreen + inline modes) remains the long-term target for extracting a standalone `crates/tui` library. This architecture is the intermediate step that fixes real bugs and adds test infrastructure while keeping ion's unique rendering model.

---

## 8. Implementation Order

| Step | What                                                                                                                     | Payoff                                 | Status                                                                   |
| ---- | ------------------------------------------------------------------------------------------------------------------------ | -------------------------------------- | ------------------------------------------------------------------------ |
| 1    | `text` module: `display_width`, `wrap_text`, `truncate_to_width` (`-> String`), `safe_width`                             | Single source of truth for width ops   | ✓ DONE                                                                   |
| 2    | `StyledSpan::plain_text()`, `StyledLine::plain_text()`, `StyledLine::display_width()`                                    | Unlocks all testing; no rnk touch      | ✓ DONE                                                                   |
| 3    | Split `ChatRenderer::build_lines` into 5 functions using `text::wrap_text`                                               | Each path unit-testable                | ✓ DONE                                                                   |
| 4    | Write tests for chat renderer (user msg, wrap, tool calls, code blocks)                                                  | Regression safety before Step 5        | ✓ DONE                                                                   |
| 5    | `ansi` module using `crossterm::style::ContentStyle`; reimplement `write_to`/`write_to_width`; add `to_ansi`; remove rnk | Remove rnk dependency                  | ✓ DONE                                                                   |
| 6    | Composer: remove `last_width`, add `width` param to `move_up`/`move_down`/visual nav methods                             | Fix resize glitch                      | ✓ DONE                                                                   |
| 7    | Bottom UI: row-string `Buffer` with `diff` + `to_plain_lines`; extract formatting helpers from paint_row                 | Testable helpers; partial flicker work | ✓ DONE (partial — row-string buffer, not full cell buffer from lib spec) |
| 8    | Write bottom UI snapshot tests for formatting helpers                                                                    | Regression safety                      | ✓ DONE                                                                   |

**Gap vs lib spec**: Step 7 implemented a simpler row-string buffer. The full cell-based `Buffer { cells: Vec<Cell> }` with Taffy layout and diff-based live rendering described in `tui-lib-spec.md` §5 is NOT done — that's part of the future `crates/tui/` library build.

Steps 1–4 are pure additions (no existing behavior changes). Steps 5–8 are replacements.

---

## 9. Key Decisions

**ropey stays** — `Vec<String>` in the lib spec's `InputState` is insufficient for ion's composer. Ropey's O(log n) operations matter for large pastes. The blob/placeholder system is ion-specific.

**Scrollback model stays** — chat history in terminal scrollback is correct for a coding agent. Buffer/diff for the bottom UI only.

**No Taffy yet** — layout.rs row arithmetic is working and tested. Taffy adds value when the lib is extracted. For now, the `Region { row, height }` model stays.

**rnk removed** — replaced by the `ansi` module which uses crossterm's `ContentStyle` / `StyledContent<D: Display>`. crossterm is already in the dependency tree; `StyledContent` implements `Display` correctly with proper reset handling. No hand-rolled escape code tables.

**`wrap_text` owns wrapping** — chat renderer and composer both delegate to `text::wrap_text`. No divergence on CJK or emoji boundaries.
