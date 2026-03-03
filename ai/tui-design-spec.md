# TUI Library Design Specification

> A general-purpose, high-performance terminal UI library for Rust.
> Designed to support applications ranging from simple input bars to
> full multiplexer-tier tools (Zellij, Neovim, btop complexity).
> Ion is the primary first consumer and development driver.

---

## Table of Contents

1. [Goals & Non-Goals](#1-goals--non-goals)
2. [Crate & Workspace Layout](#2-crate--workspace-layout)
3. [Dependency Surface](#3-dependency-surface)
4. [Core Types](#4-core-types)
5. [Buffer Layer](#5-buffer-layer)
6. [Terminal Layer](#6-terminal-layer)
7. [Layout Layer](#7-layout-layer)
8. [Event System](#8-event-system)
9. [App Trait & Effect System](#9-app-trait--effect-system)
10. [Event Loop](#10-event-loop)
11. [Widget System](#11-widget-system)
12. [Built-in Widgets](#12-built-in-widgets)
13. [Render Modes](#13-render-modes)
14. [Styling & Theming](#14-styling--theming)
15. [Testing Infrastructure](#15-testing-infrastructure)
16. [Error Handling](#16-error-handling)
17. [Implementation Phases](#17-implementation-phases)
18. [Open Questions](#18-open-questions)

---

## 1. Goals & Non-Goals

### Goals

- Own the full stack from raw terminal bytes to composable widgets
- First-class inline mode and fullscreen mode, both fully correct
- Tokio-native async event loop with no blocking
- Taffy (flexbox/grid) for layout — no custom constraint solver
- Double-buffered cell diffing for minimal terminal writes
- Correct unicode: grapheme clusters, wide chars (CJK, emoji)
- Reactive dirty-flag rendering — only repaint changed regions
- Composable widget system with explicit state ownership
- Snapshot testing built in from day one
- No ratatui dependency — we own the buffer and diff layer

### Non-Goals (for now)

- Sixel / image rendering
- Hyperlink support
- IPC / multi-client session attach (tmux-style)
- PTY hosting / embedded terminal emulator
- WebAssembly / xterm.js backend
- Windows support in v0 (crossterm handles it, but don't actively test)

---

## 2. Crate & Workspace Layout

```
workspace/
├── Cargo.toml                  ← workspace root
├── crates/
│   └── tui/                    ← the library (no ion knowledge)
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── buffer.rs
│           ├── terminal.rs
│           ├── layout.rs
│           ├── event.rs
│           ├── app.rs
│           ├── style.rs
│           ├── geometry.rs
│           └── widgets/
│               ├── mod.rs
│               ├── text.rs
│               ├── block.rs
│               ├── list.rs
│               ├── input.rs
│               ├── scroll.rs
│               ├── row.rs
│               ├── col.rs
│               └── canvas.rs
└── ion/
    ├── Cargo.toml              ← depends on tui as path dep
    └── src/
        └── ui/                 ← ion-specific widgets built on tui
            ├── conversation.rs
            ├── streaming.rs
            ├── code_block.rs
            ├── tool_call.rs
            ├── diff_view.rs
            └── status_bar.rs
```

### Cargo.toml (workspace root)

```toml
[workspace]
members = ["crates/tui", "ion"]
resolver = "2"
```

### Cargo.toml (crates/tui)

```toml
[package]
name = "tui"                    # rename before publishing
version = "0.1.0"
edition = "2021"
rust-version = "1.75"

[dependencies]
crossterm      = "0.28"
taffy          = "0.7"
tokio          = { version = "1", features = ["full"] }
unicode-segmentation = "1.12"
unicode-width  = "0.2"
bitflags       = "2"

[dev-dependencies]
tokio          = { version = "1", features = ["full", "test-util"] }
```

---

## 3. Dependency Surface

| Crate | Role | Notes |
|---|---|---|
| `crossterm` | Terminal I/O, raw mode, event stream | Kept as implementation detail — never leaked into public API |
| `taffy` | Flexbox / grid layout | Used in `layout.rs`, exposed via our own constraint types |
| `tokio` | Async runtime, event loop | Required. No sync alternative planned. |
| `unicode-segmentation` | Grapheme cluster iteration | Required for correct cursor math |
| `unicode-width` | Cell width for CJK / wide chars | Required for buffer writes |
| `bitflags` | `KeyModifiers` | Minor |

**Explicitly excluded:** ratatui, rnk, any other TUI framework.

---

## 4. Core Types

### geometry.rs

```rust
/// A position in the terminal grid (zero-indexed, col/row)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    pub x: u16,
    pub y: u16,
}

/// A rectangular region of the terminal
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self;
    pub fn area(&self) -> u32;
    pub fn is_empty(&self) -> bool;
    pub fn contains(&self, pos: Position) -> bool;
    pub fn intersection(&self, other: Rect) -> Option<Rect>;
    pub fn inner(&self, margin: u16) -> Rect;

    /// Clamp a rect to fit within a boundary
    pub fn clamp(&self, bounds: Rect) -> Rect;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}

impl Size {
    pub fn new(width: u16, height: u16) -> Self;
}
```

---

## 5. Buffer Layer

### buffer.rs

The buffer is the core rendering primitive. Everything is written into a
`Buffer` then diffed against the previous frame. Only changed cells
produce terminal output.

```rust
/// A single terminal cell
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// The grapheme cluster in this cell. Usually a single char.
    /// For wide chars (width=2), the second cell contains '\0' as a sentinel.
    pub symbol: String,
    pub style: Style,
    /// 1 for normal, 2 for wide (CJK, some emoji). Filled by set_string.
    pub width: u8,
    /// True if this is the second cell of a wide char (sentinel cell)
    pub skip: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            symbol: " ".to_string(),
            style: Style::default(),
            width: 1,
            skip: false,
        }
    }
}

impl Cell {
    pub fn reset(&mut self);
    pub fn set_symbol(&mut self, s: &str) -> &mut Self;
    pub fn set_style(&mut self, style: Style) -> &mut Self;
}

/// A 2D grid of cells — the output of a render pass
pub struct Buffer {
    pub area: Rect,
    cells: Vec<Cell>,
}

impl Buffer {
    pub fn new(area: Rect) -> Self;
    pub fn empty(area: Rect) -> Self;
    pub fn filled(area: Rect, cell: Cell) -> Self;

    /// Index into cells by (x, y) — panics if out of bounds in debug, clamps in release
    pub fn index(&self, x: u16, y: u16) -> usize;
    pub fn get(&self, x: u16, y: u16) -> &Cell;
    pub fn get_mut(&mut self, x: u16, y: u16) -> &mut Cell;

    /// Write a styled string at (x, y), respecting grapheme width.
    /// Clips at right edge of area. Fills wide char sentinel cells.
    /// Returns the x position after the last written grapheme.
    pub fn set_string(&mut self, x: u16, y: u16, s: &str, style: Style) -> u16;

    /// Write a single styled grapheme cluster
    pub fn set_symbol(&mut self, x: u16, y: u16, symbol: &str, style: Style);

    /// Write a string truncated to max_width grapheme columns
    pub fn set_string_truncated(
        &mut self,
        x: u16,
        y: u16,
        s: &str,
        max_width: u16,
        style: Style,
    ) -> u16;

    /// Fill a rectangular region with a cell value
    pub fn fill_region(&mut self, area: Rect, cell: &Cell);

    /// Merge another buffer into self, overwriting cells
    pub fn merge(&mut self, other: &Buffer);

    /// Reset all cells to default
    pub fn reset(&mut self);

    /// Produce the minimal sequence of draw commands to
    /// transform `prev` into `self`. Coalesces adjacent
    /// same-style runs into single writes.
    pub fn diff(&self, prev: &Buffer) -> Vec<DrawCommand>;

    /// Convert to a Vec of plain strings (for testing)
    pub fn to_lines(&self) -> Vec<String>;

    /// Convert to lines preserving ANSI style codes (for richer testing)
    pub fn to_styled_lines(&self) -> Vec<StyledLine>;
}

/// A minimal draw instruction produced by diff()
/// Consumed by Terminal::flush_commands()
#[derive(Debug)]
pub(crate) enum DrawCommand {
    MoveTo(u16, u16),
    SetStyle(Style),
    Print(String),
    ResetStyle,
}
```

#### Unicode correctness requirements

- Use `unicode_segmentation::UnicodeSegmentation::graphemes(s, true)` for iteration
- Use `unicode_width::UnicodeWidthStr::width(g)` per grapheme for cell width
- Wide chars (width=2): write grapheme to cell N, write sentinel (skip=true, symbol="\0") to cell N+1
- When rendering diff, skip sentinel cells — terminal cursor is already past them
- If a wide char would overflow the line, fill both cells with spaces

---

## 6. Terminal Layer

### terminal.rs

Owns raw mode, the alternate screen, cursor visibility, and terminal I/O.
Crossterm is used here and nowhere else in the public API.

```rust
pub struct Terminal {
    /// crossterm backend — never exposed publicly
    backend: CrosstermBackend,
    size: Size,
    mode: RenderMode,
    cursor_visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Own the full terminal screen (alternate buffer)
    Fullscreen,
    /// Render inline at current cursor position, N lines max
    Inline { height: u16 },
}

impl Terminal {
    /// Initialize terminal. Enters raw mode, hides cursor.
    /// Does NOT enter alternate screen — that's done by run() based on mode.
    pub fn new(mode: RenderMode) -> Result<Self>;

    /// Current terminal size
    pub fn size(&self) -> Size;

    /// Flush a set of DrawCommands produced by Buffer::diff()
    pub fn flush_commands(&mut self, commands: Vec<DrawCommand>) -> Result<()>;

    /// Show or hide the cursor
    pub fn set_cursor_visible(&mut self, visible: bool) -> Result<()>;

    /// Position the cursor (for input widget use)
    pub fn set_cursor_position(&mut self, pos: Position) -> Result<()>;

    /// Restore terminal to pre-run state:
    /// - Leave alternate screen if fullscreen
    /// - Show cursor
    /// - Disable raw mode
    /// - In inline mode: move cursor below rendered region
    pub fn restore(self) -> Result<()>;

    /// Handle a resize event — updates internal size
    pub(crate) fn handle_resize(&mut self, width: u16, height: u16);

    /// Switch between modes at runtime (e.g. enter fullscreen from inline)
    pub fn switch_mode(&mut self, mode: RenderMode) -> Result<()>;
}
```

#### Inline mode specifics

Inline mode is significantly more complex than fullscreen. The terminal
must track:

- `start_row`: the terminal row where the UI began rendering
- `rendered_height`: how many rows are currently occupied
- On each render: move cursor to `start_row`, render buffer, track new height
- On resize: if new width changed line wrapping, recompute `start_row`
- On restore: move cursor to `start_row + rendered_height`, print newline

The key invariant: the terminal's scrollback history above `start_row`
is never touched. Scrollback below is overwritten each frame.

---

## 7. Layout Layer

### layout.rs

A thin, ergonomic wrapper around Taffy. Users express layout in terms of
our types; Taffy internals are hidden.

```rust
use taffy::prelude as taffy;

/// Layout constraints for a widget node
#[derive(Debug, Clone, Default)]
pub struct LayoutStyle {
    pub direction: Direction,
    pub size: LayoutSize,
    pub min_size: LayoutSize,
    pub max_size: LayoutSize,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Dimension,
    pub align_self: Option<Align>,
    pub align_items: Option<Align>,
    pub justify_content: Option<Justify>,
    pub gap: (Gap, Gap),    // (column_gap, row_gap)
    pub padding: Edges,
    pub margin: Edges,
    pub position: PositionType,
    pub inset: Edges,       // for absolute positioning
    pub display: Display,
    pub overflow: (Overflow, Overflow),
}

#[derive(Debug, Clone, Copy, Default)]
pub enum Direction {
    #[default]
    Row,
    Column,
    RowReverse,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LayoutSize {
    pub width: Dimension,
    pub height: Dimension,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum Dimension {
    #[default]
    Auto,
    /// Fixed terminal cells
    Cells(u16),
    /// Fraction of available space (flex)
    Fraction(f32),
    /// Percentage of parent
    Percent(f32),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Edges {
    pub top: Dimension,
    pub right: Dimension,
    pub bottom: Dimension,
    pub left: Dimension,
}

impl Edges {
    pub fn all(v: u16) -> Self;
    pub fn horizontal(v: u16) -> Self;
    pub fn vertical(v: u16) -> Self;
    pub fn symmetric(h: u16, v: u16) -> Self;
}

/// The result of a layout pass — maps widget IDs to screen Rects
pub struct Layout {
    rects: HashMap<WidgetId, Rect>,
}

impl Layout {
    pub fn get(&self, id: WidgetId) -> Option<Rect>;
    pub fn get_unchecked(&self, id: WidgetId) -> Rect;
}

/// Performs a layout pass using Taffy over a widget tree.
/// Returns a Layout mapping WidgetId → Rect.
pub fn compute_layout(tree: &WidgetTree, available: Size) -> Layout;

/// Internal: translates our Dimension → taffy::Dimension
fn to_taffy_dim(d: Dimension) -> taffy::Dimension;
```

---

## 8. Event System

### event.rs

All terminal events and app-level messages flow through a single unified
type. Crossterm types are translated here and never exposed.

```rust
/// All events the app can receive
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    // Terminal events
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
    // Tick for time-driven UI (animations, spinners)
    // Only fired if App::tick_rate() returns Some(duration)
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub kind: KeyEventKind,     // Press, Repeat, Release
}

impl KeyEvent {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self;
    pub fn plain(code: KeyCode) -> Self;
    pub fn ctrl(c: char) -> Self;
    pub fn alt(c: char) -> Self;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    F(u8),
    Backspace,
    Delete,
    Enter,
    Left, Right, Up, Down,
    Home, End,
    PageUp, PageDown,
    Tab, BackTab,
    Insert,
    Esc,
    Null,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct KeyModifiers: u8 {
        const SHIFT   = 0b00000001;
        const CTRL    = 0b00000010;
        const ALT     = 0b00000100;
        const SUPER   = 0b00001000;
        const NONE    = 0b00000000;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventKind {
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    pub column: u16,
    pub row: u16,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEventKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    Moved,
    ScrollDown,
    ScrollUp,
    ScrollLeft,
    ScrollRight,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseButton { Left, Right, Middle }

/// Internal: translate a crossterm Event to our Event type
pub(crate) fn translate_event(ev: crossterm::event::Event) -> Option<Event>;
```

---

## 9. App Trait & Effect System

### app.rs

The `App` trait is what library users implement. It follows a
message-passing architecture (similar to Bubbletea / Elm) rather than
React hooks. This is more testable, scales better to complex state, and
doesn't require a runtime hook system.

```rust
/// The trait users implement to build an application
pub trait App: Sized + Send + 'static {
    /// The message type for this app. Produced by event handlers,
    /// commands, and external async tasks.
    type Message: Send + 'static;

    /// Handle a message and return an effect.
    /// This is the only place state mutation happens.
    /// Must be fast — no blocking I/O.
    fn update(&mut self, msg: Self::Message) -> Effect<Self::Message>;

    /// Produce the current UI tree.
    /// Called after every update() that returned a non-None effect.
    /// Must be pure — no side effects, no state mutation.
    fn view(&self) -> Element;

    /// Translate a raw terminal event into an app message.
    /// Return None to ignore the event.
    fn handle_event(&self, event: &Event) -> Option<Self::Message>;

    /// Optional: rate at which Tick events are sent.
    /// Return None (default) to disable tick events entirely.
    fn tick_rate(&self) -> Option<std::time::Duration> {
        None
    }

    /// Optional: called once before the event loop starts.
    /// Return an Effect to kick off initial commands (e.g. load data).
    fn init(&mut self) -> Effect<Self::Message> {
        Effect::None
    }

    /// Optional: called after the event loop ends.
    fn on_exit(&mut self) {}
}

/// Effects are the mechanism for side effects and async work.
/// They're returned from update() and executed by the runtime.
pub enum Effect<Msg> {
    /// No side effect
    None,

    /// Signal the event loop to exit cleanly
    Quit,

    /// Run multiple effects
    Batch(Vec<Effect<Msg>>),

    /// Run an async task; the result is fed back as a message
    Command(Pin<Box<dyn Future<Output = Msg> + Send>>),

    /// Emit a message immediately (next update tick)
    Emit(Msg),
}

impl<Msg: Send + 'static> Effect<Msg> {
    /// Convenience: wrap a future as a Command
    pub fn command<F>(f: F) -> Self
    where
        F: Future<Output = Msg> + Send + 'static,
    {
        Effect::Command(Box::pin(f))
    }

    /// Convenience: batch builder
    pub fn batch(effects: impl IntoIterator<Item = Effect<Msg>>) -> Self {
        Effect::Batch(effects.into_iter().collect())
    }
}
```

---

## 10. Event Loop

### app.rs (continued) — AppRunner

The runtime that drives an `App`. Constructed by `AppBuilder`, consumed
by `.run()`.

```rust
pub struct AppBuilder<A: App> {
    app: A,
    mode: RenderMode,
    mouse_capture: bool,
    focus_events: bool,
    bracketed_paste: bool,
}

impl<A: App> AppBuilder<A> {
    pub fn new(app: A) -> Self;
    pub fn inline(mut self, height: u16) -> Self;
    pub fn fullscreen(mut self) -> Self;
    pub fn mouse(mut self, enabled: bool) -> Self;
    pub fn focus_events(mut self, enabled: bool) -> Self;
    pub fn bracketed_paste(mut self, enabled: bool) -> Self;

    pub async fn run(self) -> Result<A>;
}

/// Internal runner — not pub
struct AppRunner<A: App> {
    app: A,
    terminal: Terminal,
    prev_buf: Buffer,
    msg_tx: mpsc::UnboundedSender<A::Message>,
    msg_rx: mpsc::UnboundedReceiver<A::Message>,
    dirty: bool,
}

impl<A: App> AppRunner<A> {
    async fn run_loop(mut self) -> Result<A> {
        // Handle init effect
        let init_effect = self.app.init();
        self.execute_effect(init_effect).await;

        // Initial render
        self.render()?;

        let mut event_stream = crossterm::event::EventStream::new();
        let mut tick_interval = self.app.tick_rate()
            .map(|d| tokio::time::interval(d));
        // 60fps render ceiling
        let mut render_interval = tokio::time::interval(
            std::time::Duration::from_millis(16)
        );
        render_interval.set_missed_tick_behavior(
            tokio::time::MissedTickBehavior::Skip
        );

        loop {
            tokio::select! {
                // App messages (from commands, external senders)
                Some(msg) = self.msg_rx.recv() => {
                    let effect = self.app.update(msg);
                    let quit = matches!(effect, Effect::Quit);
                    self.execute_effect(effect).await;
                    self.dirty = true;
                    if quit { break; }
                }

                // Terminal events
                Some(Ok(ev)) = event_stream.next() => {
                    if let Some(ev) = translate_event(ev) {
                        // Handle resize at framework level
                        if let Event::Resize(w, h) = ev {
                            self.terminal.handle_resize(w, h);
                            self.dirty = true;
                        }
                        // Ctrl+C always quits unless app handles it first
                        let handled = if let Some(msg) = self.app.handle_event(&ev) {
                            let effect = self.app.update(msg);
                            let quit = matches!(effect, Effect::Quit);
                            self.execute_effect(effect).await;
                            self.dirty = true;
                            if quit { break; }
                            true
                        } else {
                            false
                        };
                        if !handled {
                            if let Event::Key(k) = &ev {
                                if k.code == KeyCode::Char('c')
                                    && k.modifiers.contains(KeyModifiers::CTRL)
                                {
                                    break;
                                }
                            }
                        }
                    }
                }

                // Tick events (if enabled)
                Some(_) = tick(tick_interval.as_mut()) => {
                    if let Some(msg) = self.app.handle_event(&Event::Tick) {
                        let effect = self.app.update(msg);
                        let quit = matches!(effect, Effect::Quit);
                        self.execute_effect(effect).await;
                        self.dirty = true;
                        if quit { break; }
                    }
                }

                // Render tick — only if dirty
                _ = render_interval.tick() => {
                    if self.dirty {
                        self.render()?;
                        self.dirty = false;
                    }
                }
            }
        }

        self.terminal.restore()?;
        self.app.on_exit();
        Ok(self.app)
    }

    fn render(&mut self) -> Result<()> {
        let size = self.terminal.size();
        let area = Rect::new(0, 0, size.width, size.height);
        let mut buf = Buffer::new(area);

        // Build element tree from view
        let root_element = self.app.view();

        // Layout pass
        let layout = compute_layout(&root_element, size);

        // Render pass
        root_element.render(&layout, area, &mut buf);

        // Diff and flush
        let commands = buf.diff(&self.prev_buf);
        self.terminal.flush_commands(commands)?;
        self.prev_buf = buf;
        Ok(())
    }

    async fn execute_effect(&self, effect: Effect<A::Message>) {
        match effect {
            Effect::None => {}
            Effect::Quit => {} // handled by caller
            Effect::Emit(msg) => {
                let _ = self.msg_tx.send(msg);
            }
            Effect::Command(fut) => {
                let tx = self.msg_tx.clone();
                tokio::spawn(async move {
                    let msg = fut.await;
                    let _ = tx.send(msg);
                });
            }
            Effect::Batch(effects) => {
                for e in effects {
                    self.execute_effect(e).await;
                }
            }
        }
    }
}

/// Helper for optional interval
async fn tick(interval: Option<&mut tokio::time::Interval>) -> Option<tokio::time::Instant> {
    match interval {
        Some(i) => Some(i.tick().await),
        None => std::future::pending().await,
    }
}
```

---

## 11. Widget System

### widgets/mod.rs

Widgets produce `Element` values. An `Element` is a node in the render
tree. The system is **not** a virtual DOM — there is no reconciler or
retained tree between frames. `view()` builds a fresh tree every frame,
layout is computed, then cells are written to the buffer. The double
buffer diff catches unchanged regions.

This keeps the mental model simple while the buffer diff handles
performance. For cases where full-tree rebuild per frame is too
expensive (e.g. a 10,000-line conversation view), the `Canvas` widget
provides an escape hatch with custom rendering logic.

```rust
/// A unique identifier for a widget node, used by Layout
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetId(u64);

impl WidgetId {
    pub fn new() -> Self;  // generates unique ID
}

/// A node in the render tree.
/// Produced by widget builder methods (e.g. Text::new("hi").into_element())
pub struct Element {
    pub(crate) id: WidgetId,
    pub(crate) inner: Box<dyn Renderable>,
    pub(crate) layout_style: LayoutStyle,
    pub(crate) children: Vec<Element>,
}

impl Element {
    /// Apply layout style overrides to this element
    pub fn width(mut self, w: Dimension) -> Self;
    pub fn height(mut self, h: Dimension) -> Self;
    pub fn flex_grow(mut self, v: f32) -> Self;
    pub fn flex_shrink(mut self, v: f32) -> Self;
    pub fn flex_basis(mut self, v: Dimension) -> Self;
    pub fn padding(mut self, edges: Edges) -> Self;
    pub fn margin(mut self, edges: Edges) -> Self;
    pub fn min_width(mut self, w: Dimension) -> Self;
    pub fn min_height(mut self, h: Dimension) -> Self;
    pub fn max_width(mut self, w: Dimension) -> Self;
    pub fn max_height(mut self, h: Dimension) -> Self;

    /// Render this element and all children into a buffer
    pub(crate) fn render(&self, layout: &Layout, area: Rect, buf: &mut Buffer);
}

/// Trait implemented by widget internals
pub(crate) trait Renderable: Send + Sync {
    fn render(&self, area: Rect, buf: &mut Buffer);
}

/// Conversion helper — widgets implement this
pub trait IntoElement {
    fn into_element(self) -> Element;
}
```

---

## 12. Built-in Widgets

### 12.1 Text — widgets/text.rs

```rust
pub struct Text {
    content: Vec<Span>,   // A line may have multiple styled spans
    alignment: Alignment,
    wrap: WrapMode,
    style: Style,
}

pub struct Span {
    pub content: String,
    pub style: Style,
}

#[derive(Default, Clone, Copy)]
pub enum Alignment { Left, Center, Right, #[default] Left }

#[derive(Default, Clone, Copy)]
pub enum WrapMode {
    #[default]
    Word,    // break at word boundaries
    Char,    // break at any character
    None,    // clip to width
}

impl Text {
    pub fn new(s: impl Into<String>) -> Self;
    pub fn spans(spans: Vec<Span>) -> Self;
    pub fn styled(s: impl Into<String>, style: Style) -> Self;
    pub fn alignment(mut self, a: Alignment) -> Self;
    pub fn wrap(mut self, w: WrapMode) -> Self;
    pub fn style(mut self, s: Style) -> Self;
    pub fn bold(mut self) -> Self;
    pub fn dim(mut self) -> Self;
    pub fn italic(mut self) -> Self;
    pub fn underline(mut self) -> Self;
}

impl IntoElement for Text { ... }
```

### 12.2 Block — widgets/block.rs

Container with border, title, and padding. Renders its child in the
inner area.

```rust
pub struct Block {
    title: Option<Vec<Span>>,
    title_position: TitlePosition,
    border_type: BorderType,
    border_style: Style,
    style: Style,       // background fill
    child: Option<Element>,
}

#[derive(Default, Clone, Copy)]
pub enum BorderType {
    #[default] Rounded,
    Plain,
    Double,
    Thick,
    None,
}

#[derive(Default, Clone, Copy)]
pub enum TitlePosition { #[default] TopLeft, TopCenter, TopRight }

impl Block {
    pub fn new() -> Self;
    pub fn title(mut self, title: impl Into<String>) -> Self;
    pub fn title_spans(mut self, spans: Vec<Span>) -> Self;
    pub fn border(mut self, border_type: BorderType) -> Self;
    pub fn border_style(mut self, style: Style) -> Self;
    pub fn style(mut self, style: Style) -> Self;
    pub fn child(mut self, child: Element) -> Self;
}

impl IntoElement for Block { ... }
```

### 12.3 Row / Col — widgets/row.rs, col.rs

Flex containers. Sugar over `Element` with direction set.

```rust
pub struct Row {
    children: Vec<Element>,
    gap: u16,
    align_items: Align,
    justify_content: Justify,
}

impl Row {
    pub fn new(children: Vec<Element>) -> Self;
    pub fn gap(mut self, gap: u16) -> Self;
    pub fn align(mut self, a: Align) -> Self;
    pub fn justify(mut self, j: Justify) -> Self;
}

impl IntoElement for Row { ... }
// Col is identical with Direction::Column
```

### 12.4 List — widgets/list.rs

Virtual-scrolling list with variable-height items. This is the
performance-critical widget for ion's conversation view.

```rust
pub struct List {
    items: Vec<Element>,
    state: ListState,
    scroll_bar: bool,
}

/// State owned externally (in App::Model)
#[derive(Debug, Clone, Default)]
pub struct ListState {
    /// Index of the topmost visible item
    pub offset: usize,
    /// Currently selected index (for keyboard navigation)
    pub selected: Option<usize>,
}

impl ListState {
    pub fn select(&mut self, i: usize);
    pub fn select_next(&mut self, item_count: usize);
    pub fn select_prev(&mut self);
    pub fn scroll_to_bottom(&mut self, item_count: usize);
    /// Ensure the selected item is visible in viewport
    pub fn ensure_visible(&mut self, viewport_height: u16, item_heights: &[u16]);
}

impl List {
    pub fn new(items: Vec<Element>) -> Self;
    pub fn state(mut self, state: &ListState) -> Self;
    pub fn scroll_bar(mut self, show: bool) -> Self;
}

impl IntoElement for List { ... }
```

**Virtual scroll implementation notes:**

The list widget computes item heights during layout (Taffy gives us
this). It renders only items that overlap the viewport by maintaining
`offset` into the items vec. On scroll events, `ListState::offset` is
updated by the app, triggering a re-render. Items outside the viewport
are not rendered at all.

### 12.5 Input — widgets/input.rs

Multiline text input with real terminal keybindings. This is the most
complex built-in widget.

```rust
pub struct Input {
    state: InputState,
    placeholder: Option<String>,
    style: Style,
    focused: bool,
}

/// State owned externally (in App::Model)
#[derive(Debug, Clone, Default)]
pub struct InputState {
    /// Lines of content. Always at least one line.
    lines: Vec<String>,
    /// Cursor: (line_index, grapheme_index_in_line)
    cursor: (usize, usize),
    /// Kill ring for Ctrl+K / Ctrl+Y
    kill_buffer: String,
    /// Input history (for Up/Down navigation)
    history: Vec<String>,
    history_pos: Option<usize>,
}

impl InputState {
    pub fn new() -> Self;
    pub fn value(&self) -> String;                  // all lines joined with \n
    pub fn set_value(&mut self, s: &str);
    pub fn clear(&mut self);
    pub fn push_history(&mut self, entry: String);

    // Key handling — returns true if the key was consumed
    pub fn handle_key(&mut self, key: &KeyEvent) -> InputAction;
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputAction {
    /// Key was handled, state changed
    Changed,
    /// Key was handled, no state change (cursor moved but content same)
    Navigated,
    /// User pressed Enter (submit)
    Submit,
    /// User pressed Shift+Enter or Ctrl+Enter (newline)
    Newline,
    /// Key was not handled by input widget
    Unhandled,
}
```

**Required keybindings:**

| Key | Action |
|---|---|
| `Char(c)` | Insert character at cursor |
| `Enter` | Submit |
| `Shift+Enter` | Insert newline |
| `Backspace` | Delete char before cursor |
| `Delete` | Delete char after cursor |
| `Left / Right` | Move cursor by grapheme |
| `Up / Down` | Move cursor line, or history navigate |
| `Home / Ctrl+A` | Beginning of line |
| `End / Ctrl+E` | End of line |
| `Ctrl+Left / Alt+B` | Word back |
| `Ctrl+Right / Alt+F` | Word forward |
| `Ctrl+W` | Delete word back |
| `Alt+D` | Delete word forward |
| `Ctrl+U` | Delete to beginning of line |
| `Ctrl+K` | Kill to end of line (into kill buffer) |
| `Ctrl+Y` | Yank from kill buffer |
| `Ctrl+A` (line start) | Move to beginning |
| `Ctrl+H` | Backspace (alt) |

Word boundary: whitespace-delimited. Do not use regex.

### 12.6 Scroll — widgets/scroll.rs

Wraps any element, adding a scrollable viewport and optional scrollbar.

```rust
pub struct Scroll {
    child: Element,
    state: ScrollState,
    direction: ScrollDirection,
    bar: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ScrollState {
    pub offset: u16,
    pub content_length: u16,   // set by layout pass
    pub viewport_length: u16,  // set by layout pass
}

impl ScrollState {
    pub fn scroll_up(&mut self, delta: u16);
    pub fn scroll_down(&mut self, delta: u16);
    pub fn scroll_to_top(&mut self);
    pub fn scroll_to_bottom(&mut self);
    pub fn scroll_by_page(&mut self, viewport: u16);
}

#[derive(Clone, Copy, Default)]
pub enum ScrollDirection { #[default] Vertical, Horizontal, Both }
```

### 12.7 Overlay — widgets/overlay.rs

Renders a child element on top of a base element. Used for modals,
dropdowns, tooltips, command palettes.

```rust
pub struct Overlay {
    base: Element,
    overlay: Element,
    position: OverlayPosition,
}

pub enum OverlayPosition {
    /// Center of the base
    Center,
    /// Fixed offset from a corner
    TopLeft { x: u16, y: u16 },
    TopRight { x: u16, y: u16 },
    BottomLeft { x: u16, y: u16 },
    BottomRight { x: u16, y: u16 },
    /// Absolute terminal coordinates
    Absolute(Position),
}
```

### 12.8 Canvas — widgets/canvas.rs

Escape hatch for custom rendering. The closure receives the area and
buffer directly and can do whatever it wants. No layout children.

```rust
pub struct Canvas {
    render_fn: Box<dyn Fn(Rect, &mut Buffer) + Send + Sync>,
}

impl Canvas {
    pub fn new(f: impl Fn(Rect, &mut Buffer) + Send + Sync + 'static) -> Self;
}

impl IntoElement for Canvas { ... }
```

Ion uses this for `StreamingText`, `CodeBlock`, and `DiffView` — widgets
with complex internal rendering that can't be expressed as child elements.

---

## 13. Render Modes

Two modes, both fully supported with correct behavior:

### Fullscreen

- Enters alternate screen buffer (`smcup`) on start
- App owns the full terminal
- Cursor hidden by default
- Leaves alternate screen (`rmcup`) on exit — terminal content restored
- Resize: re-render at new size immediately

### Inline

- App renders at current cursor position
- Output scrolls up as needed if content grows
- Terminal scrollback above the UI is never touched
- Input cursor is positioned by the input widget
- On exit: cursor moves below rendered region, leaves output visible
- Resize: recompute line wrap, redraw — `start_row` may change
- `println()` support: print a line above the UI, shift `start_row` down

**Runtime switching:** `Terminal::switch_mode()` handles transitioning
between modes. This is how an inline input bar can expand to fullscreen
when the user wants more room.

---

## 14. Styling & Theming

### style.rs

```rust
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifiers: StyleModifiers,
}

impl Style {
    pub fn new() -> Self;
    pub fn fg(mut self, c: Color) -> Self;
    pub fn bg(mut self, c: Color) -> Self;
    pub fn bold(mut self) -> Self;
    pub fn dim(mut self) -> Self;
    pub fn italic(mut self) -> Self;
    pub fn underline(mut self) -> Self;
    pub fn strikethrough(mut self) -> Self;
    pub fn reset(mut self) -> Self;

    /// Layer self on top of other — self wins on non-None fields
    pub fn patch(self, other: Style) -> Style;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Reset,
    Black, Red, Green, Yellow, Blue, Magenta, Cyan, White,
    DarkGray, LightRed, LightGreen, LightYellow,
    LightBlue, LightMagenta, LightCyan, Gray,
    Rgb(u8, u8, u8),
    Indexed(u8),
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct StyleModifiers: u16 {
        const BOLD          = 0b000000001;
        const DIM           = 0b000000010;
        const ITALIC        = 0b000000100;
        const UNDERLINE     = 0b000001000;
        const BLINK         = 0b000010000;
        const REVERSED      = 0b000100000;
        const HIDDEN        = 0b001000000;
        const STRIKETHROUGH = 0b010000000;
    }
}
```

### Theme

A `Theme` is a struct of named `Style` values that widgets query. Ion
defines its own theme; the library ships a default.

```rust
/// Passed through the render tree via a render context (not global state)
#[derive(Debug, Clone)]
pub struct Theme {
    pub text: Style,
    pub text_dim: Style,
    pub border: Style,
    pub border_focused: Style,
    pub selected: Style,
    pub title: Style,
    pub input: Style,
    pub input_cursor: Style,
    pub scrollbar: Style,
    pub error: Style,
    pub warning: Style,
    pub success: Style,
}

impl Theme {
    pub fn default() -> Self;
    /// Load from a TOML config file
    pub fn from_toml(s: &str) -> Result<Self>;
}
```

Theme is passed into `AppBuilder` and threaded through the render context.
Widgets receive it via a `RenderContext` alongside their `Rect` and `Buffer`.

---

## 15. Testing Infrastructure

### widgets/testing.rs (re-exported from lib)

The buffer renders to plain strings, making snapshot testing trivial.

```rust
/// Render a widget tree to a buffer at the given size, return as lines
pub fn render_to_lines(element: Element, width: u16, height: u16) -> Vec<String>;

/// Render and return as ANSI-escaped string (for style assertions)
pub fn render_to_ansi(element: Element, width: u16, height: u16) -> String;

/// Assert buffer matches expected lines
#[macro_export]
macro_rules! assert_render {
    ($element:expr, $width:expr, $height:expr, $expected:expr) => {
        let lines = $crate::testing::render_to_lines($element, $width, $height);
        assert_eq!(lines, $expected, "render mismatch");
    };
}
```

**Example tests:**

```rust
#[test]
fn text_wraps_at_boundary() {
    let widget = Text::new("Hello world this is a long line").wrap(WrapMode::Word);
    let lines = render_to_lines(widget.into_element(), 12, 3);
    assert_eq!(lines, vec![
        "Hello world ",
        "this is a   ",
        "long line   ",
    ]);
}

#[test]
fn block_renders_border() {
    let widget = Block::new()
        .title("Test")
        .border(BorderType::Plain);
    let lines = render_to_lines(widget.into_element(), 10, 3);
    assert_eq!(lines, vec![
        "┌─ Test ──┐",
        "│         │",
        "└─────────┘",
    ]);
}

#[test]
fn input_handles_ctrl_k() {
    let mut state = InputState::new();
    state.set_value("hello world");
    // Move cursor to position 5
    for _ in 0..5 { state.handle_key(&KeyEvent::plain(KeyCode::Right)); }
    let action = state.handle_key(&KeyEvent::ctrl('k'));
    assert_eq!(action, InputAction::Changed);
    assert_eq!(state.value(), "hello");
    // Yank restores
    state.handle_key(&KeyEvent::ctrl('y'));
    assert_eq!(state.value(), "hello world");
}
```

---

## 16. Error Handling

```rust
/// The library's error type
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("crossterm error: {0}")]
    Crossterm(#[from] crossterm::ErrorKind),

    #[error("layout error: {0}")]
    Layout(String),

    #[error("terminal size is too small: {width}x{height} (minimum 10x4)")]
    TerminalTooSmall { width: u16, height: u16 },
}

pub type Result<T> = std::result::Result<T, TuiError>;
```

The library does not panic in release mode on bad inputs — it returns
`Err` or clamps values. Debug builds may assert invariants.

---

## 17. Implementation Phases

Build in this order to keep ion unblocked:

### Phase 1 — Foundation (Week 1–2)

`geometry.rs`, `style.rs`, `buffer.rs`, `terminal.rs`

Deliverable: Can write to a terminal buffer and flush it. Can enter/exit
raw mode and alternate screen. Manual test: print a colored grid.

### Phase 2 — Event Loop (Week 2–3)

`event.rs`, `app.rs` (App trait + AppBuilder + AppRunner)

Deliverable: A "hello world" app with App trait compiles and runs. Quit
on Ctrl+C. Resize handled.

### Phase 3 — Layout + Text (Week 3–4)

`layout.rs`, `widgets/text.rs`, `widgets/row.rs`, `widgets/col.rs`, `widgets/block.rs`

Deliverable: Taffy-driven layout. Multi-column and nested layouts work.
Text wrapping works. Snapshot tests pass.

### Phase 4 — Input (Week 4–5)

`widgets/input.rs`

Deliverable: Multiline input with all keybindings. History. Cursor
positioning in terminal. Ion can use this for its input bar immediately.

### Phase 5 — List + Scroll (Week 5–6)

`widgets/list.rs`, `widgets/scroll.rs`

Deliverable: Virtual-scrolled list. Ion's conversation view can be built
on top. Overlay widget.

### Phase 6 — Polish

`widgets/canvas.rs`, theme system, testing utilities, error types, inline
mode correctness pass, `thiserror` integration.

---

## 18. Open Questions

These should be decided before or during Phase 1:

**Crate name.** Check crates.io availability before getting attached to
anything. Avoid names that imply ion-specificity.

**`RenderContext` vs bare `Rect + Buffer`.** Do widgets receive a
`RenderContext { area, buf, theme }` or just `(area, buf)`? Context is
cleaner for theming but adds a type to the public API. Recommendation:
use context from day one — easier to add fields later than to break
signatures.

**`WidgetId` generation.** Options: sequential counter, random u64,
type-id + position hash. For Phase 1 it doesn't matter — ids are only
used for layout → rect mapping within a single frame. Sequential counter
per frame is fine.

**Public vs pub(crate) for `DrawCommand`.** Terminal internals should
stay private. Currently `DrawCommand` is `pub(crate)`. Keep it that way.

**Sync widgets.** Should `Renderable` be `Send + Sync`? Yes — required
for `view()` to be called from async context and for widgets to be held
in `App` state which is `Send`.

**`thiserror` as dependency.** Small and stable. Include it. Don't
hand-roll error impls.

**Minimum terminal size.** Return `TuiError::TerminalTooSmall` if
terminal is under 10×4. Apps can decide how to handle it.
