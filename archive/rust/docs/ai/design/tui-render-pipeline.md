# TUI Render Pipeline Design

## Problem Statement

The TUI has 6 distinct terminal output paths in `run.rs`, each computing positions independently. The state tracking (`chat_row`, `startup_ui_anchor`, `last_ui_start`, `streaming_lines_rendered`) uses scattered `Option` fields with implicit invariants that are enforced by convention, not types. Layout is computed multiple times per frame by different callers that can disagree. This has produced 4+ rounds of bug fixes, each patching symptoms rather than fixing the structural problem.

## Diagnosis

### The 6 Output Paths in run.rs

| #   | Trigger                       | Lines   | What it does                     | Position source                                       |
| --- | ----------------------------- | ------- | -------------------------------- | ----------------------------------------------------- |
| 1   | Session resume (startup)      | 244-261 | ScrollUp + reprint all chat      | `position_after_reprint`                              |
| 2   | `needs_screen_clear` (/clear) | 304-315 | ScrollUp(term_height)            | None (just scrolls)                                   |
| 3   | `needs_reflow` (resize)       | 321-359 | ScrollUp + reprint all chat      | `position_after_reprint`                              |
| 4   | `needs_selector_clear`        | 362-376 | MoveTo + Clear from selector top | `compute_layout`                                      |
| 5   | Header insertion              | 378-389 | Print header, set anchor         | `cursor::position()`                                  |
| 6   | Chat insertion + draw_direct  | 393-505 | insert_before + bottom UI        | `chat_row` / `calculate_ui_height` + `compute_layout` |

Each path manages its own subset of render state fields. Paths 1 and 3 call `position_after_reprint`. Path 6 reads `chat_row` and calls both `calculate_ui_height` (for chat insertion) and `compute_layout` (for draw_direct). These are independent computations of overlapping values.

### Implicit State Invariants (Not Enforced)

- `chat_row = None` implies `last_ui_start = None` (violated during selector close before the fix)
- `startup_ui_anchor = Some(_)` implies `header_inserted = true` and `message_list.entries.is_empty()`
- `streaming_lines_rendered > 0` implies `is_running = true` and last entry is Agent
- `needs_reflow` implies `chat_row = None` and `startup_ui_anchor = None`

### Duplicate Computations Per Frame

`calculate_ui_height` and `compute_layout` both compute the same total by summing popup + progress + input + status heights. `ui_start_row` is called by `compute_layout` but was also called independently by chat insertion code (now partially unified but `calculate_ui_height` remains as a separate entry point).

## Design

### 1. Position State Machine

Replace `chat_row: Option<u16>`, `startup_ui_anchor: Option<u16>`, `last_ui_start: Option<u16>`, and `header_inserted: bool` with an explicit enum:

```rust
/// Tracks where the chat content sits relative to the terminal viewport.
/// Encodes the positioning mode and associated row anchors.
#[derive(Debug, Clone, Copy)]
pub enum ChatPosition {
    /// Initial state: no header printed, no chat content.
    /// UI will render at bottom of screen.
    Empty,

    /// Header has been printed. UI anchors below the header.
    /// `anchor` is the row immediately after the header lines.
    /// No chat messages exist yet.
    Header { anchor: u16 },

    /// Chat content is being placed at explicit row positions.
    /// Content fits on screen; UI follows the chat.
    /// `next_row` is where the next chat line will be printed.
    /// `ui_drawn_at` tracks where draw_direct last placed the UI top
    /// (replaces `last_ui_start`).
    Tracking {
        next_row: u16,
        ui_drawn_at: Option<u16>,
    },

    /// Chat content has overflowed the viewport.
    /// Content is pushed into scrollback via ScrollUp.
    /// UI is pinned to `term_height - ui_height`.
    /// `ui_drawn_at` tracks where draw_direct last placed the UI top.
    Scrolling {
        ui_drawn_at: Option<u16>,
    },
}
```

**State transitions:**

```
Empty --[print header]--> Header { anchor }
Header { anchor } --[first chat line]--> Tracking { next_row: anchor, ui_drawn_at: None }
Tracking { next_row, .. } --[content fits]--> Tracking { next_row: next_row + lines, .. }
Tracking { next_row, .. } --[overflow]--> Scrolling { ui_drawn_at: None }
Scrolling --[stays]--> Scrolling

Any --[resize]--> requires reflow (set needs_reflow, handled by reflow path)
Any --[/clear]--> Empty (after screen clear)
Any --[session load]--> needs reflow (reprint sets Tracking or Scrolling)
```

**Accessor methods on ChatPosition:**

```rust
impl ChatPosition {
    /// Row to place the UI when using row-tracking.
    /// Returns None in Scrolling mode (use bottom-pinned layout).
    pub fn ui_anchor(&self) -> Option<u16> {
        match self {
            Self::Empty => None,
            Self::Header { anchor } => Some(*anchor),
            Self::Tracking { next_row, .. } => Some(*next_row),
            Self::Scrolling { .. } => None,
        }
    }

    /// Previous frame's UI top row, for clear_from computation.
    pub fn last_ui_top(&self) -> Option<u16> {
        match self {
            Self::Tracking { ui_drawn_at, .. } | Self::Scrolling { ui_drawn_at } => *ui_drawn_at,
            _ => None,
        }
    }

    /// Record where draw_direct placed the UI this frame.
    pub fn set_ui_drawn_at(&mut self, row: u16) {
        match self {
            Self::Tracking { ui_drawn_at, .. } | Self::Scrolling { ui_drawn_at } => {
                *ui_drawn_at = Some(row);
            }
            // In Empty/Header states, draw_direct still renders.
            // Transition to appropriate state or store ephemerally.
            Self::Empty | Self::Header { .. } => {}
        }
    }

    /// Whether the header has been printed.
    pub fn header_inserted(&self) -> bool {
        !matches!(self, Self::Empty)
    }

    /// Whether we are in row-tracking mode.
    pub fn is_tracking(&self) -> bool {
        matches!(self, Self::Tracking { .. })
    }
}
```

**What this eliminates:**

| Old field                        | Replaced by                                 |
| -------------------------------- | ------------------------------------------- |
| `chat_row: Option<u16>`          | `Tracking { next_row }` vs `Scrolling`      |
| `startup_ui_anchor: Option<u16>` | `Header { anchor }`                         |
| `last_ui_start: Option<u16>`     | `ui_drawn_at` inside `Tracking`/`Scrolling` |
| `header_inserted: bool`          | `!matches!(position, Empty)`                |

**Invalid states that become unrepresentable:**

- `chat_row = Some(_)` with `header_inserted = false` -- impossible, `Tracking` is always post-header
- `startup_ui_anchor = Some(_)` with `header_inserted = false` -- impossible, `Header` implies printed
- `chat_row = None` with `last_ui_start = Some(row)` referencing a stale tracking position -- `Scrolling` resets `ui_drawn_at` on transition
- `startup_ui_anchor = Some(_)` with non-empty message list -- `Header` transitions to `Tracking` on first message

### 2. Frame Pipeline

Each frame in the main loop follows this sequence:

```
                    +---------------------------+
                    |  1. POLL                   |
                    |  event::poll + update()    |
                    |  terminal::size()          |
                    +---------------------------+
                                |
                    +---------------------------+
                    |  2. PREPARE                |
                    |  Process flags:            |
                    |  - needs_screen_clear      |
                    |  - needs_reflow            |
                    |  - needs_selector_clear    |
                    |  - header insertion        |
                    |  Take chat inserts         |
                    +---------------------------+
                                |
                    +---------------------------+
                    |  3. PLAN                   |
                    |  compute_layout() ONCE     |
                    |  Determine what changed    |
                    |  Build FramePlan           |
                    +---------------------------+
                                |
                    +---------------------------+
                    |  4. RENDER                 |
                    |  BeginSynchronizedUpdate   |
                    |  Execute FramePlan         |
                    |  EndSynchronizedUpdate     |
                    |  Update position state     |
                    +---------------------------+
```

#### FramePlan

The plan phase produces a `FramePlan` that describes all terminal operations needed:

```rust
/// Describes all terminal output for a single frame.
/// Built during the PLAN phase, executed during RENDER.
pub struct FramePlan {
    /// Pre-render operations (clear screen, reflow, etc.)
    pub pre_ops: Vec<PreOp>,
    /// Chat lines to insert (empty if no new chat content)
    pub chat_insert: Option<ChatInsert>,
    /// Whether to redraw the bottom UI
    pub draw_ui: bool,
    /// Layout for the bottom UI (computed once)
    pub layout: UiLayout,
}

/// Operations that happen before chat insertion.
pub enum PreOp {
    /// Push viewport to scrollback, blank the screen.
    ClearScreen,
    /// Push viewport to scrollback, reprint chat at new width.
    /// Contains pre-rendered lines to print.
    Reflow(Vec<StyledLine>),
    /// Clear the area where the selector was.
    ClearSelectorArea,
    /// Print the startup header.
    PrintHeader(Vec<StyledLine>),
    /// Clear the startup header area (first message arriving).
    ClearHeaderArea { from_row: u16 },
}

/// How to insert chat lines into the viewport.
pub enum ChatInsert {
    /// Print at explicit rows (row-tracking mode).
    /// After printing, position advances.
    AtRow {
        start_row: u16,
        lines: Vec<StyledLine>,
    },
    /// Transition from tracking to scrolling.
    /// Clear old UI, scroll, print in new space.
    Overflow {
        old_ui_row: u16,
        scroll_amount: u16,
        print_row: u16,
        lines: Vec<StyledLine>,
    },
    /// Already in scroll mode. Clear UI, scroll, print.
    ScrollInsert {
        ui_start: u16,
        scroll_amount: u16,
        print_row: u16,
        lines: Vec<StyledLine>,
    },
}
```

#### Why FramePlan Matters

The current code interleaves decision-making and rendering. For example, the chat insertion block in `run.rs:421-493` reads `chat_row`, decides which mode to use, then immediately executes terminal commands. If `compute_layout` is called later (line 496), it may see different state because the chat insertion already mutated `chat_row` and `last_ui_start`.

With FramePlan, the entire frame is planned with consistent state, then executed atomically. Position state updates happen after execution, not during.

### 3. RenderState Redesign

```rust
pub struct RenderState {
    /// Position state machine (replaces chat_row, startup_ui_anchor,
    /// last_ui_start, header_inserted).
    pub position: ChatPosition,

    /// Number of chat entries already committed to scrollback.
    pub rendered_entries: usize,

    /// Buffered chat lines while selector is open.
    pub buffered_chat_lines: Vec<StyledLine>,

    /// Lines from streaming agent entry already committed.
    /// Reset when entry finishes, tool call interrupts, or reflow occurs.
    pub streaming_lines_rendered: usize,

    // --- One-shot flags (set by event handlers, consumed by frame pipeline) ---

    /// Clear visible screen (/clear command).
    pub needs_screen_clear: bool,
    /// Re-render chat at new width (resize).
    pub needs_reflow: bool,
    /// Clear selector area without full repaint.
    pub needs_selector_clear: bool,
    /// Force render on first frame after state change.
    pub needs_initial_render: bool,
}
```

**Removed fields:**

- `chat_row` -> `ChatPosition::Tracking { next_row }`
- `startup_ui_anchor` -> `ChatPosition::Header { anchor }`
- `last_ui_start` -> `ChatPosition::*.ui_drawn_at`
- `header_inserted` -> `ChatPosition::header_inserted()`

**Reset methods become position transitions:**

```rust
impl RenderState {
    pub fn reset_for_new_conversation(&mut self) {
        self.position = ChatPosition::Empty;
        self.rendered_entries = 0;
        self.buffered_chat_lines.clear();
        self.streaming_lines_rendered = 0;
        self.needs_initial_render = true;
    }

    pub fn reset_for_session_load(&mut self) {
        self.position = ChatPosition::Empty;
        self.rendered_entries = 0;
        self.buffered_chat_lines.clear();
        self.streaming_lines_rendered = 0;
        self.needs_initial_render = true;
    }

    /// After reflow: set position based on how many lines were printed.
    pub fn position_after_reprint(
        &mut self,
        line_count: usize,
        term_height: u16,
        ui_height: u16,
    ) -> u16 {
        let available = term_height.saturating_sub(ui_height) as usize;
        if line_count <= available {
            self.position = ChatPosition::Tracking {
                next_row: line_count as u16,
                ui_drawn_at: None,
            };
            0
        } else {
            let excess = (line_count
                .min(term_height as usize)
                .saturating_sub(available)) as u16;
            self.position = ChatPosition::Scrolling { ui_drawn_at: None };
            excess
        }
    }

    pub fn mark_reflow_complete(&mut self, entries: usize) {
        self.rendered_entries = entries;
        self.buffered_chat_lines.clear();
        self.streaming_lines_rendered = 0;
    }
}
```

**Resize handler becomes:**

```rust
Event::Resize(_, _) => {
    self.input_state.invalidate_width();
    // Position is invalid after terminal reflow.
    // Reflow will reprint and set the correct position.
    self.render_state.needs_reflow = true;
    // Don't touch position here -- reflow will set it.
}
```

This removes the scattered `chat_row = None; startup_ui_anchor = None; last_ui_start = None;` reset in the resize handler. The reflow path is the single place that resets position after a resize.

### 4. Output Path Unification

The 6 output paths collapse into the frame pipeline phases:

#### PREPARE phase (in `run.rs` main loop)

```rust
fn prepare_frame(
    app: &mut App,
    term_width: u16,
    term_height: u16,
) -> FramePrep {
    let mut pre_ops = Vec::new();
    let mut position_changed = false;

    // 1. Screen clear (/clear)
    if app.render_state.needs_screen_clear {
        app.render_state.needs_screen_clear = false;
        pre_ops.push(PreOp::ClearScreen);
        position_changed = true;
    }

    // 2. Reflow (resize)
    if app.render_state.needs_reflow {
        app.render_state.needs_reflow = false;
        if !app.message_list.entries.is_empty() {
            let lines = app.build_chat_lines(term_width);
            pre_ops.push(PreOp::Reflow(lines));
        } else {
            // Reset to empty -- header will be re-inserted
            app.render_state.position = ChatPosition::Empty;
        }
        position_changed = true;
    }

    // 3. Selector clear
    if app.render_state.needs_selector_clear {
        app.render_state.needs_selector_clear = false;
        pre_ops.push(PreOp::ClearSelectorArea);
        position_changed = true;
    }

    // 4. Header insertion
    if !app.render_state.position.header_inserted() {
        let header_lines = app.take_startup_header_lines();
        if !header_lines.is_empty() {
            pre_ops.push(PreOp::PrintHeader(header_lines));
            position_changed = true;
        }
    }

    // 5. Chat content
    let chat_lines = app.take_chat_inserts(term_width);

    // 6. First-message: clear header area
    if !chat_lines.is_empty() {
        if let ChatPosition::Header { anchor } = app.render_state.position {
            pre_ops.push(PreOp::ClearHeaderArea { from_row: anchor });
        }
    }

    FramePrep {
        pre_ops,
        chat_lines,
        position_changed,
    }
}
```

#### PLAN phase

```rust
fn plan_frame(
    app: &App,
    prep: FramePrep,
    term_width: u16,
    term_height: u16,
    had_event: bool,
    was_running: bool,
) -> Option<FramePlan> {
    let needs_render = prep.position_changed
        || had_event
        || app.is_running
        || was_running
        || !prep.chat_lines.is_empty()
        || app.render_state.needs_initial_render;

    if !needs_render {
        return None;
    }

    // Compute layout ONCE for the entire frame
    let layout = app.compute_layout(
        term_width,
        term_height,
        app.render_state.position.last_ui_top(),
    );

    let chat_insert = if prep.chat_lines.is_empty() {
        None
    } else {
        Some(plan_chat_insert(
            &app.render_state.position,
            prep.chat_lines,
            &layout,
            term_height,
        ))
    };

    Some(FramePlan {
        pre_ops: prep.pre_ops,
        chat_insert,
        draw_ui: true,
        layout,
    })
}

fn plan_chat_insert(
    position: &ChatPosition,
    lines: Vec<StyledLine>,
    layout: &UiLayout,
    term_height: u16,
) -> ChatInsert {
    let line_count = lines.len() as u16;
    let ui_height = term_height.saturating_sub(layout.top);

    match position {
        ChatPosition::Tracking { next_row, .. } | ChatPosition::Header { anchor: next_row } => {
            let space_needed = next_row
                .saturating_add(line_count)
                .saturating_add(ui_height);
            if space_needed <= term_height {
                ChatInsert::AtRow {
                    start_row: *next_row,
                    lines,
                }
            } else {
                let content_end = next_row.saturating_add(line_count);
                let ui_start = term_height.saturating_sub(ui_height);
                let scroll_amount = content_end.saturating_sub(ui_start);
                let print_row = ui_start.saturating_sub(line_count);
                ChatInsert::Overflow {
                    old_ui_row: *next_row,
                    scroll_amount,
                    print_row,
                    lines,
                }
            }
        }
        ChatPosition::Scrolling { .. } | ChatPosition::Empty => {
            let ui_start = term_height.saturating_sub(ui_height);
            ChatInsert::ScrollInsert {
                ui_start,
                scroll_amount: line_count,
                print_row: ui_start.saturating_sub(line_count),
                lines,
            }
        }
    }
}
```

#### RENDER phase

```rust
fn render_frame(
    stdout: &mut io::Stdout,
    app: &mut App,
    plan: FramePlan,
    term_width: u16,
    term_height: u16,
) -> io::Result<()> {
    // Pre-ops that must happen outside synchronized update
    // (e.g., clear header area before sync block)
    for op in &plan.pre_ops {
        if let PreOp::ClearHeaderArea { from_row } = op {
            execute!(stdout, MoveTo(0, *from_row), Clear(ClearType::FromCursorDown))?;
            stdout.flush()?;
        }
    }

    execute!(stdout, BeginSynchronizedUpdate)?;

    // Execute pre-ops
    for op in &plan.pre_ops {
        match op {
            PreOp::ClearScreen => {
                execute!(
                    stdout,
                    crossterm::terminal::ScrollUp(term_height),
                    MoveTo(0, 0)
                )?;
            }
            PreOp::Reflow(lines) => {
                execute!(
                    stdout,
                    crossterm::terminal::ScrollUp(term_height),
                    MoveTo(0, 0)
                )?;
                for line in lines {
                    line.writeln(stdout)?;
                }
                let ui_height = term_height.saturating_sub(plan.layout.top);
                let excess = app.render_state.position_after_reprint(
                    lines.len(),
                    term_height,
                    ui_height,
                );
                if excess > 0 {
                    execute!(stdout, crossterm::terminal::ScrollUp(excess))?;
                }
                // Update rendered_entries
                let mut end = app.message_list.entries.len();
                if app.is_running
                    && app.message_list.entries.last()
                        .is_some_and(|e| e.sender == Sender::Agent)
                {
                    end = end.saturating_sub(1);
                }
                app.render_state.mark_reflow_complete(end);
            }
            PreOp::ClearSelectorArea => {
                execute!(
                    stdout,
                    MoveTo(0, plan.layout.top),
                    Clear(ClearType::FromCursorDown)
                )?;
            }
            PreOp::PrintHeader(lines) => {
                for line in lines {
                    line.writeln(stdout)?;
                }
                if let Ok((_x, y)) = crossterm::cursor::position() {
                    app.render_state.position = ChatPosition::Header { anchor: y };
                }
            }
            PreOp::ClearHeaderArea { .. } => {
                // Already handled outside sync block
            }
        }
    }

    // Execute chat insertion
    if let Some(insert) = &plan.chat_insert {
        match insert {
            ChatInsert::AtRow { start_row, lines } => {
                for (i, line) in lines.iter().enumerate() {
                    execute!(
                        stdout,
                        MoveTo(0, start_row.saturating_add(i as u16)),
                        Clear(ClearType::CurrentLine)
                    )?;
                    line.writeln(stdout)?;
                }
                let new_row = start_row.saturating_add(lines.len() as u16);
                app.render_state.position = ChatPosition::Tracking {
                    next_row: new_row,
                    ui_drawn_at: None,
                };
            }
            ChatInsert::Overflow {
                old_ui_row,
                scroll_amount,
                print_row,
                lines,
            } => {
                execute!(
                    stdout,
                    MoveTo(0, *old_ui_row),
                    Clear(ClearType::FromCursorDown)
                )?;
                execute!(stdout, crossterm::terminal::ScrollUp(*scroll_amount))?;
                for (i, line) in lines.iter().enumerate() {
                    execute!(
                        stdout,
                        MoveTo(0, print_row.saturating_add(i as u16)),
                        Clear(ClearType::CurrentLine)
                    )?;
                    line.writeln(stdout)?;
                }
                app.render_state.position = ChatPosition::Scrolling { ui_drawn_at: None };
            }
            ChatInsert::ScrollInsert {
                ui_start,
                scroll_amount,
                print_row,
                lines,
            } => {
                execute!(
                    stdout,
                    MoveTo(0, *ui_start),
                    Clear(ClearType::FromCursorDown)
                )?;
                execute!(stdout, crossterm::terminal::ScrollUp(*scroll_amount))?;
                let mut row = *print_row;
                for line in lines {
                    execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
                    line.writeln(stdout)?;
                    row = row.saturating_add(1);
                }
            }
        }
    }

    // Draw bottom UI
    if plan.draw_ui {
        app.draw_direct(stdout, &plan.layout)?;
    }

    execute!(stdout, EndSynchronizedUpdate)?;
    stdout.flush()?;

    // Clear the initial render flag after successful render
    app.render_state.needs_initial_render = false;

    Ok(())
}
```

#### Main Loop (simplified)

```rust
loop {
    // 1. POLL
    let had_event = poll_event(&mut app, &mut term_width, &mut term_height)?;
    let was_running = app.is_running;
    app.update();

    // 2. PREPARE
    let prep = prepare_frame(&mut app, term_width, term_height);

    // 3. PLAN
    let plan = plan_frame(&app, prep, term_width, term_height, had_event, was_running);

    // 4. RENDER (or skip if nothing changed)
    if let Some(plan) = plan {
        render_frame(&mut stdout, &mut app, plan, term_width, term_height)?;
    }

    if app.should_quit {
        break;
    }

    // Handle editor request (outside frame pipeline -- suspends TUI)
    handle_editor_request(&mut app, &mut stdout, supports_enhancement)?;
}
```

### 5. Eliminate calculate_ui_height

`calculate_ui_height` is the redundant twin of `compute_layout`. After this redesign, it is deleted. Any code that needs the UI height derives it from `UiLayout`:

```rust
impl UiLayout {
    /// Total height of the UI area.
    pub fn height(&self) -> u16 {
        match &self.body {
            BodyLayout::Input { status, .. } => {
                status.row + status.height - self.top
            }
            BodyLayout::Selector { selector } => {
                selector.height
            }
        }
    }
}
```

All callers of `calculate_ui_height` in run.rs (resume, reflow, chat insertion) use `layout.height()` from the single `compute_layout` call instead.

For the resume path (before the main loop), `compute_layout` is called once to get the UI height for `position_after_reprint`.

### 6. File-Level Changes

| File                           | Change                                                                                                                                                                                                       | Effort |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------ |
| `src/tui/render_state.rs`      | Replace 4 fields with `ChatPosition` enum. Update reset methods. Delete `header_inserted` field.                                                                                                             | Medium |
| `src/tui/render/layout.rs`     | Delete `calculate_ui_height` and `ui_start_row` methods. Add `UiLayout::height()`. Update `compute_layout` to take `ChatPosition` reference instead of `last_top: Option<u16>`.                              | Medium |
| `src/tui/run.rs`               | Replace inline output paths with `prepare_frame` / `plan_frame` / `render_frame` functions. Main loop becomes 15 lines. Delete ~200 lines of inline rendering.                                               | Large  |
| `src/tui/render/direct.rs`     | `draw_direct` now writes `ui_drawn_at` via `position.set_ui_drawn_at(layout.top)` instead of `self.render_state.last_ui_start = Some(layout.top)`.                                                           | Small  |
| `src/tui/render/chat.rs`       | `reprint_chat_scrollback` unchanged in behavior, but callers change. `take_chat_inserts` unchanged.                                                                                                          | Small  |
| `src/tui/mod.rs`               | Delete `header_inserted()`, `set_startup_ui_anchor()`, `take_startup_ui_anchor()` accessor methods. Position is accessed directly via `render_state.position`.                                               | Small  |
| `src/tui/events.rs`            | Resize handler simplified to just `needs_reflow = true`. Selector close: set `needs_selector_clear`, no manual position reset.                                                                               | Small  |
| `src/tui/session/lifecycle.rs` | No change (already calls `reset_for_session_load`).                                                                                                                                                          | None   |
| `src/tui/session/tasks.rs`     | No change.                                                                                                                                                                                                   | None   |
| `src/tui/input.rs`             | `take_startup_header_lines` checks `position.header_inserted()` instead of `header_inserted` field. Sets `header_inserted` via position transition (or keep the take pattern and have prepare_frame use it). | Small  |

### 7. compute_layout Signature Change

```rust
// Before:
pub fn compute_layout(&self, width: u16, height: u16, last_top: Option<u16>) -> UiLayout

// After:
pub fn compute_layout(&self, width: u16, height: u16) -> UiLayout
```

The `last_top` parameter was a leak of render state into the layout API. Instead, `compute_layout` reads `self.render_state.position.last_ui_top()` directly (it already reads `self.render_state.chat_row` via `ui_start_row`). This is not a purity regression -- the method already reads `self` for mode, picker state, completer state, and input height. Having it also read position state is consistent.

The `ui_start_row` helper method inside `compute_layout` changes to:

```rust
fn ui_start_row(&self, height: u16, ui_height: u16) -> u16 {
    let bottom_start = height.saturating_sub(ui_height);
    match self.render_state.position.ui_anchor() {
        Some(anchor) => anchor.min(bottom_start),
        None => bottom_start,
    }
}
```

### 8. Migration Strategy

**Clean break, not incremental.** The position state machine cannot coexist with the old fields because they encode the same information differently. An incremental approach would require maintaining both representations in sync, which is exactly the kind of implicit invariant this design eliminates.

#### Implementation sequence (single PR, 3-4 commits):

**Commit 1: ChatPosition enum + RenderState field replacement**

- Add `ChatPosition` enum to `render_state.rs`
- Replace `chat_row`, `startup_ui_anchor`, `last_ui_start`, `header_inserted` with `position: ChatPosition`
- Update all reset methods
- Update all reads of the old fields to use the new enum
- This commit will have many changes but each is mechanical (pattern match instead of Option check)
- Build should compile but behavior is equivalent

**Commit 2: Frame pipeline functions**

- Add `prepare_frame`, `plan_frame`, `render_frame` as free functions in `run.rs`
- Add `FramePlan`, `PreOp`, `ChatInsert` types (can be in `run.rs` or a new `frame.rs`)
- Replace the inline main loop body with calls to these functions
- Delete `calculate_ui_height` and `ui_start_row` standalone methods
- Update `compute_layout` signature

**Commit 3: Cleanup**

- Delete accessor methods on `App` (`header_inserted()`, `set_startup_ui_anchor()`, `take_startup_ui_anchor()`)
- Update tests for new `RenderState` shape
- Delete any dead code

#### Risk mitigation

- The position state machine (commit 1) can be verified by running the existing test suite -- all `RenderState` tests exercise the same transitions
- The frame pipeline (commit 2) should be tested by manual verification of: startup header, first message, short chat, overflow transition, resize, /clear, session resume, selector open/close
- Add unit tests for `plan_chat_insert` with known inputs -- this is pure arithmetic, easily testable
- The existing `compute_layout` tests in `layout.rs` continue to work since `UiLayout` and `Region` are unchanged

## Invariants Enforced by the New Design

| Invariant                                               | How enforced                                                 |
| ------------------------------------------------------- | ------------------------------------------------------------ |
| Header printed before tracking starts                   | `Empty -> Header -> Tracking` transitions                    |
| UI position tracked only when content exists            | `ui_drawn_at` only on `Tracking` and `Scrolling`             |
| Resize invalidates all positioning                      | Single `needs_reflow = true` instead of 3 field resets       |
| Layout computed once per frame                          | `plan_frame` computes layout, passes to `render_frame`       |
| Chat insertion and UI drawing use same layout           | Both receive the same `UiLayout` from the plan               |
| Position state updated after rendering, not during      | `render_frame` updates position at the end of each operation |
| Reflow is the only path that sets position after resize | `needs_reflow` handler calls `position_after_reprint`        |

## What Does Not Change

- `StyledLine` and the chat rendering pipeline (`ChatRenderer`, `build_chat_lines`, etc.)
- `draw_direct` internals (input, status, progress, popup, selector rendering)
- `UiLayout`, `Region`, `BodyLayout` types
- The hybrid model: chat in scrollback, UI at bottom
- Synchronized updates bracketing
- The insert_before pattern for chat content
- Event handling logic (events.rs)
- Agent task management (session/tasks.rs, session/update.rs)
