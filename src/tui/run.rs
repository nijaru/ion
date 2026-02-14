//! TUI main loop and terminal management.
//!
//! This module contains the entry point for the TUI, terminal setup/cleanup,
//! and the main event/render loop. The loop follows a prepare-plan-render
//! pipeline: flags are consumed into a `FramePrep`, chat insertion is planned
//! as pure arithmetic via `plan_chat_insert`, and everything is rendered
//! atomically inside a synchronized update.

use crate::cli::PermissionSettings;
use crate::tui::App;
use crate::tui::message_list::{MessageEntry, Sender};
use crate::tui::render::layout::UiLayout;
use crate::tui::render_state::ChatPosition;
use crate::tui::terminal::StyledLine;
use crate::tui::types::Mode;
use anyhow::Result;
use crossterm::{
    cursor::{MoveTo, Show},
    event::{
        self, DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, disable_raw_mode,
        enable_raw_mode, supports_keyboard_enhancement,
    },
};
use std::io::{self, Write};

// ---------------------------------------------------------------------------
// Frame pipeline types
// ---------------------------------------------------------------------------

/// Operations that happen before chat insertion.
enum PreOp {
    /// Push visible content to scrollback, blank the screen.
    ClearScreen {
        scroll_amount: u16,
        full_clear: bool,
    },
    /// Clear screen + scrollback and re-render all chat from the data model.
    FullRerender,
    /// Scroll viewport content up to make room for a taller bottom UI
    /// without reprinting prior chat lines.
    ScrollViewport { scroll_amount: u16 },
    /// Clear the area where the selector was.
    ClearSelectorArea { from_row: u16 },
    /// Print the startup header.
    PrintHeader(Vec<StyledLine>),
    /// Clear the startup header area (first message arriving).
    ClearHeaderArea { from_row: u16 },
}

/// How to insert chat lines into the viewport.
enum ChatInsert {
    /// Print at explicit rows (row-tracking mode).
    AtRow {
        start_row: u16,
        lines: Vec<StyledLine>,
    },
    /// Transition from tracking to scrolling.
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

/// Collected pre-render operations and chat content for a frame.
struct FramePrep {
    pre_ops: Vec<PreOp>,
    chat_lines: Vec<StyledLine>,
    state_changed: bool,
}

fn rendered_entry_count(app: &App) -> usize {
    let mut end = app.message_list.entries.len();
    if app.is_running
        && app
            .message_list
            .entries
            .last()
            .is_some_and(|entry| entry.sender == Sender::Agent)
    {
        end = end.saturating_sub(1);
    }
    end
}

fn scroll_up_and_home(stdout: &mut io::Stdout, amount: u16) -> io::Result<()> {
    execute!(stdout, crossterm::terminal::ScrollUp(amount), MoveTo(0, 0))
}

fn append_lines(stdout: &mut io::Stdout, lines: &[StyledLine]) -> io::Result<()> {
    for line in lines {
        line.writeln(stdout)?;
    }
    Ok(())
}

fn append_lines_clipped(
    stdout: &mut io::Stdout,
    lines: &[StyledLine],
    term_width: u16,
) -> io::Result<()> {
    for line in lines {
        line.writeln_with_width(stdout, term_width)?;
    }
    Ok(())
}

#[allow(clippy::cast_possible_truncation)]
fn write_lines_at(
    stdout: &mut io::Stdout,
    start_row: u16,
    lines: &[StyledLine],
    term_width: u16,
) -> io::Result<u16> {
    let mut row = start_row;
    for line in lines {
        execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        line.write_to_width(stdout, term_width)?;
        row = row.saturating_add(1);
    }
    Ok(row)
}

fn clear_header_areas(stdout: &mut io::Stdout, pre_ops: &[PreOp]) -> io::Result<()> {
    for op in pre_ops {
        if let PreOp::ClearHeaderArea { from_row } = op {
            execute!(
                stdout,
                MoveTo(0, *from_row),
                Clear(ClearType::FromCursorDown)
            )?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn apply_pre_ops(
    stdout: &mut io::Stdout,
    app: &mut App,
    pre_ops: &[PreOp],
    term_width: u16,
    term_height: u16,
) -> io::Result<()> {
    for op in pre_ops {
        match op {
            PreOp::ClearScreen {
                scroll_amount,
                full_clear,
            } => {
                if *full_clear {
                    execute!(stdout, MoveTo(0, 0), Clear(ClearType::All), MoveTo(0, 0))?;
                } else {
                    scroll_up_and_home(stdout, *scroll_amount)?;
                }
            }
            PreOp::FullRerender => {
                execute!(
                    stdout,
                    Clear(ClearType::All),
                    Clear(ClearType::Purge),
                    MoveTo(0, 0)
                )?;
                let line_count = app.reprint_chat_scrollback(stdout, term_width)?;
                let ui_height = app.compute_layout(term_width, term_height).height();
                let available_rows = term_height.saturating_sub(ui_height);
                if (line_count as u16) <= available_rows {
                    app.render_state.position = ChatPosition::Tracking {
                        next_row: line_count as u16,
                        ui_drawn_at: None,
                    };
                } else {
                    app.render_state.position = ChatPosition::Scrolling { ui_drawn_at: None };
                }
            }
            PreOp::ScrollViewport { scroll_amount } => {
                if *scroll_amount > 0 {
                    scroll_up_and_home(stdout, *scroll_amount)?;
                }
            }
            PreOp::ClearSelectorArea { from_row } => {
                execute!(
                    stdout,
                    MoveTo(0, *from_row),
                    Clear(ClearType::FromCursorDown)
                )?;
            }
            PreOp::PrintHeader(lines) => {
                append_lines_clipped(stdout, lines, term_width)?;
                if let Ok((_x, y)) = crossterm::cursor::position() {
                    app.render_state.position = ChatPosition::Header { anchor: y };
                }
            }
            PreOp::ClearHeaderArea { .. } => {}
        }
    }
    Ok(())
}

fn apply_chat_insert(
    stdout: &mut io::Stdout,
    app: &mut App,
    chat_insert: Option<ChatInsert>,
) -> io::Result<()> {
    let Some(insert) = chat_insert else {
        return Ok(());
    };

    match insert {
        ChatInsert::AtRow { start_row, lines } => {
            let (term_width, _) = terminal::size()?;
            let new_row = write_lines_at(stdout, start_row, &lines, term_width)?;
            // Don't set ui_drawn_at here — draw_direct does that after
            // recomputing layout with the updated next_row.
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
                MoveTo(0, old_ui_row),
                Clear(ClearType::FromCursorDown)
            )?;
            execute!(stdout, crossterm::terminal::ScrollUp(scroll_amount))?;
            let (term_width, _) = terminal::size()?;
            write_lines_at(stdout, print_row, &lines, term_width)?;
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
                MoveTo(0, ui_start),
                Clear(ClearType::FromCursorDown)
            )?;
            execute!(stdout, crossterm::terminal::ScrollUp(scroll_amount))?;
            let (term_width, _) = terminal::size()?;
            write_lines_at(stdout, print_row, &lines, term_width)?;
            app.render_state.position = ChatPosition::Scrolling { ui_drawn_at: None };
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Frame pipeline functions
// ---------------------------------------------------------------------------

/// Process flags, take chat inserts, build pre-render operations.
fn prepare_frame(app: &mut App, term_width: u16, term_height: u16) -> FramePrep {
    let mut pre_ops = Vec::new();
    let mut state_changed = false;
    let mut reflow_scheduled = false;
    let reflow_requested = app.render_state.needs_reflow;

    // Consume initial render flag
    if app.render_state.needs_initial_render {
        app.render_state.needs_initial_render = false;
        state_changed = true;
    }

    let layout = app.compute_layout(term_width, term_height);
    let ui_height = layout.height();
    let scroll_amount = app
        .render_state
        .position
        .scroll_amount(ui_height, term_height);

    // If bottom UI growth (multiline input/popup changes) would overlap tracked chat rows,
    // scroll just enough rows into scrollback to keep chat single-copy and preserve
    // native terminal history. Avoid transcript reprints here.
    let overlap_rows = if tracking_ui_overlap(&app.render_state.position, &layout) {
        tracking_ui_overlap_rows(&app.render_state.position, &layout)
    } else {
        0
    };
    if should_scroll_for_overlap(
        app.mode,
        reflow_requested,
        overlap_rows,
        !app.message_list.entries.is_empty(),
    ) {
        pre_ops.push(PreOp::ScrollViewport {
            scroll_amount: overlap_rows,
        });
        if let ChatPosition::Tracking { next_row, .. } = app.render_state.position {
            app.render_state.position = ChatPosition::Tracking {
                next_row: next_row.saturating_sub(overlap_rows),
                ui_drawn_at: None,
            };
        }
        state_changed = true;
    }

    // Screen clear (/clear)
    if app.render_state.needs_screen_clear {
        app.render_state.needs_screen_clear = false;
        pre_ops.push(PreOp::ClearScreen {
            scroll_amount,
            full_clear: matches!(app.render_state.position, ChatPosition::Empty),
        });
        state_changed = true;
    }

    // Reflow: full redraw on session load or terminal resize.
    // Debounce resize to avoid rendering at intermediate dimensions.
    if app.render_state.needs_reflow {
        let settled = match app.render_state.last_resize_at {
            Some(at) => at.elapsed() >= std::time::Duration::from_millis(16),
            None => true, // non-resize reflow (session load) — render immediately
        };
        if settled {
            app.render_state.needs_reflow = false;
            app.render_state.last_resize_at = None;
            if !app.message_list.entries.is_empty()
                || app.render_state.position.header_inserted()
            {
                pre_ops.push(PreOp::FullRerender);
                reflow_scheduled = true;
            } else {
                app.render_state.position = ChatPosition::Empty;
            }
        }
        state_changed = true;
    }

    // Selector clear
    if app.render_state.needs_selector_clear {
        if reflow_scheduled {
            // Reflow redraws chat/UI for the full frame; selector-only clear would
            // run after reflow and can wipe freshly reprinted content.
            app.render_state.cancel_selector_clear();
        } else {
            let fallback = layout.top;
            let from_row = app.render_state.take_selector_clear_from(fallback);
            app.render_state.needs_selector_clear = false;
            pre_ops.push(PreOp::ClearSelectorArea { from_row });
            state_changed = true;
        }
    }

    // Header insertion
    if !reflow_scheduled
        && app.message_list.entries.is_empty()
        && !app.render_state.position.header_inserted()
    {
        let header_lines = app.take_startup_header_lines();
        if !header_lines.is_empty() {
            pre_ops.push(PreOp::PrintHeader(header_lines));
            state_changed = true;
        }
    }

    // Chat content
    let chat_lines = if reflow_scheduled {
        Vec::new()
    } else {
        app.take_chat_inserts(term_width)
    };

    // First-message: clear header area
    if !chat_lines.is_empty()
        && let ChatPosition::Header { anchor } = app.render_state.position
    {
        pre_ops.push(PreOp::ClearHeaderArea { from_row: anchor });
    }

    FramePrep {
        pre_ops,
        chat_lines,
        state_changed,
    }
}

fn tracking_ui_overlap(position: &ChatPosition, layout: &UiLayout) -> bool {
    tracking_ui_overlap_rows(position, layout) > 0
}

fn tracking_ui_overlap_rows(position: &ChatPosition, layout: &UiLayout) -> u16 {
    match position {
        ChatPosition::Tracking { next_row, .. } if layout.top < *next_row => {
            next_row.saturating_sub(layout.top)
        }
        _ => 0,
    }
}

fn should_scroll_for_overlap(
    mode: Mode,
    reflow_requested: bool,
    overlap_rows: u16,
    has_entries: bool,
) -> bool {
    mode != Mode::Selector && !reflow_requested && overlap_rows > 0 && has_entries
}

/// Pure arithmetic to decide how to insert chat lines.
#[allow(clippy::cast_possible_truncation)]
fn plan_chat_insert(
    position: &ChatPosition,
    lines: Vec<StyledLine>,
    layout: &UiLayout,
    term_height: u16,
) -> ChatInsert {
    let line_count = lines.len() as u16;
    let ui_height = layout.height();

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
        ChatPosition::Empty => {
            let space_needed = line_count.saturating_add(ui_height);
            if space_needed <= term_height {
                ChatInsert::AtRow {
                    start_row: 0,
                    lines,
                }
            } else {
                let ui_start = term_height.saturating_sub(ui_height);
                let scroll_amount = line_count.saturating_sub(ui_start);
                let print_row = ui_start.saturating_sub(line_count);
                ChatInsert::Overflow {
                    old_ui_row: 0,
                    scroll_amount,
                    print_row,
                    lines,
                }
            }
        }
        ChatPosition::Scrolling { .. } => {
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

/// Execute all terminal operations atomically, then update position state.
fn render_frame(
    stdout: &mut io::Stdout,
    app: &mut App,
    pre_ops: Vec<PreOp>,
    chat_insert: Option<ChatInsert>,
    layout: &UiLayout,
    term_width: u16,
    term_height: u16,
) -> io::Result<()> {
    // ClearHeaderArea must happen outside sync block (avoids flicker)
    clear_header_areas(stdout, &pre_ops)?;

    execute!(stdout, BeginSynchronizedUpdate)?;
    apply_pre_ops(stdout, app, &pre_ops, term_width, term_height)?;

    // Clear stale UI rows between old top and current top (e.g., popup dismiss).
    // Must happen BEFORE chat insertion: new chat lines may occupy these rows.
    // draw_direct only clears from its current top down to preserve chat above.
    if layout.clear_from < layout.top {
        for row in layout.clear_from..layout.top {
            execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        }
    }

    apply_chat_insert(stdout, app, chat_insert)?;

    // Recompute layout after chat insertion — position may have changed.
    let post_layout = app.compute_layout(term_width, term_height);
    app.draw_direct(stdout, &post_layout)?;

    execute!(stdout, EndSynchronizedUpdate)?;
    stdout.flush()?;

    Ok(())
}

fn reprint_loaded_session(
    stdout: &mut io::Stdout,
    app: &mut App,
    term_width: u16,
    term_height: u16,
) -> io::Result<()> {
    if app.message_list.entries.is_empty() {
        return Ok(());
    }

    let layout = app.compute_layout(term_width, term_height);
    let ui_height = layout.height();
    let lines = app.build_chat_lines(term_width);

    // Print chat lines naturally from the current cursor position.
    // The terminal scrolls content into scrollback as needed — same
    // as how fresh-start prints the header.
    execute!(stdout, BeginSynchronizedUpdate)?;
    append_lines(stdout, &lines)?;

    // Check where cursor landed to set position state.
    let (_, cursor_y) = crossterm::cursor::position()?;
    let available = term_height.saturating_sub(ui_height);

    if cursor_y >= available {
        // Chat fills past the UI zone — scroll to make room for the
        // bottom UI, then enter scroll mode.
        let excess = cursor_y.saturating_sub(available);
        if excess > 0 {
            execute!(stdout, crossterm::terminal::ScrollUp(excess))?;
        }
        app.render_state.position = ChatPosition::Scrolling { ui_drawn_at: None };
    } else {
        app.render_state.position = ChatPosition::Tracking {
            next_row: cursor_y,
            ui_drawn_at: None,
        };
    }

    app.render_state
        .mark_reflow_complete(rendered_entry_count(app));
    execute!(stdout, EndSynchronizedUpdate)?;
    stdout.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Resume option & terminal setup
// ---------------------------------------------------------------------------

/// Resume option for TUI mode.
#[derive(Debug, Clone)]
pub enum ResumeOption {
    None,
    Latest,
    ById(String),
    Selector,
}

/// Terminal state returned from setup.
struct TerminalState {
    stdout: io::Stdout,
    supports_enhancement: bool,
}

/// Setup terminal for TUI mode (raw mode, bracketed paste, keyboard enhancement).
fn setup_terminal() -> Result<TerminalState> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnableBracketedPaste, EnableFocusChange)?;

    let supports_enhancement = supports_keyboard_enhancement().unwrap_or(false);
    if supports_enhancement {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    Ok(TerminalState {
        stdout,
        supports_enhancement,
    })
}

/// Handle resume option, loading session or opening selector.
/// Returns Err if session load fails fatally (`ById` with invalid id).
fn handle_resume(
    app: &mut App,
    resume_option: ResumeOption,
    supports_enhancement: bool,
) -> Result<()> {
    match resume_option {
        ResumeOption::None => {}
        ResumeOption::Latest => {
            let cwd = std::env::current_dir()
                .unwrap_or_default()
                .display()
                .to_string();
            match app.store.list_recent_for_dir(&cwd, 1) {
                Ok(sessions) => {
                    if let Some(session) = sessions.first() {
                        if let Err(e) = app.load_session(&session.id) {
                            app.message_list.push_entry(MessageEntry::new(
                                Sender::System,
                                format!("Error: Failed to load session: {e}"),
                            ));
                        }
                    } else {
                        app.message_list.push_entry(MessageEntry::new(
                            Sender::System,
                            "No recent sessions in this directory.".to_string(),
                        ));
                    }
                }
                Err(e) => {
                    app.message_list.push_entry(MessageEntry::new(
                        Sender::System,
                        format!("Error: Failed to list sessions: {e}"),
                    ));
                }
            }
        }
        ResumeOption::ById(id) => {
            if let Err(e) = app.load_session(&id) {
                let mut stdout = io::stdout();
                let _ = execute!(stdout, DisableBracketedPaste, DisableFocusChange);
                if supports_enhancement {
                    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
                }
                let _ = disable_raw_mode();
                eprintln!("Error: Session '{id}' not found: {e}");
                return Err(e);
            }
        }
        ResumeOption::Selector => {
            app.open_session_selector();
        }
    }
    Ok(())
}

/// Cleanup terminal and print session ID on exit.
fn cleanup_terminal(
    app: &App,
    stdout: &mut io::Stdout,
    supports_enhancement: bool,
    term_width: u16,
    term_height: u16,
) -> Result<()> {
    // Ensure synchronized update mode is ended (safety net if error interrupted the main loop)
    let _ = execute!(stdout, EndSynchronizedUpdate);

    // Clear UI area before exit
    let layout = app.compute_layout(term_width, term_height);
    execute!(
        stdout,
        MoveTo(0, layout.top),
        Clear(ClearType::FromCursorDown)
    )?;
    // Position cursor at layout top (just after chat content)
    execute!(stdout, MoveTo(0, layout.top))?;

    // Restore terminal
    execute!(stdout, DisableBracketedPaste, DisableFocusChange)?;
    if supports_enhancement {
        execute!(stdout, PopKeyboardEnhancementFlags)?;
    }
    disable_raw_mode()?;
    execute!(stdout, Show)?;

    // Only print session ID if there were user messages
    let has_user_messages = app
        .message_list
        .entries
        .iter()
        .any(|e| e.sender == Sender::User);
    if has_user_messages {
        StyledLine::dim(format!("Session: {}", app.session.id)).writeln(stdout)?;
    }

    Ok(())
}

/// Open text in external editor, returns edited content or None if unchanged/cancelled
fn open_editor(initial: &str) -> Result<Option<String>> {
    use std::process::Command;

    // Get editor from environment (VISUAL for full-screen, EDITOR as fallback)
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| anyhow::anyhow!("No editor configured. Set VISUAL or EDITOR environment variable.\nExample: export VISUAL=nano"))?;

    // Create temp file with initial content
    let mut temp = tempfile::NamedTempFile::with_suffix(".md")?;
    temp.write_all(initial.as_bytes())?;
    temp.flush()?;

    // Open editor - split command and args (handles "code --wait", "nvim -u NONE", etc.)
    let parts: Vec<&str> = editor.split_whitespace().collect();
    let (cmd, args) = parts
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("Empty editor command"))?;
    let status = Command::new(cmd)
        .args(args.iter())
        .arg(temp.path())
        .status()?;

    if !status.success() {
        return Ok(None);
    }

    // Read back edited content
    let edited = std::fs::read_to_string(temp.path())?;

    // Return None if unchanged
    if edited == initial {
        Ok(None)
    } else {
        Ok(Some(edited))
    }
}

/// Guard that restores the original panic hook on drop.
struct PanicHookGuard {
    original_hook: std::sync::Arc<dyn Fn(&std::panic::PanicHookInfo) + Send + Sync + 'static>,
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        let original_hook = std::sync::Arc::clone(&self.original_hook);
        std::panic::set_hook(Box::new(move |info| {
            (original_hook)(info);
        }));
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Main entry point for the TUI.
///
/// Sets up the terminal, creates the App, handles resume options,
/// and runs the prepare-plan-render loop until the user quits.
#[allow(clippy::too_many_lines)]
pub async fn run(permissions: PermissionSettings, resume_option: ResumeOption) -> Result<()> {
    // Set panic hook to restore terminal on panic (guard restores original on exit)
    let original_hook: std::sync::Arc<dyn Fn(&std::panic::PanicHookInfo) + Send + Sync> =
        std::sync::Arc::from(std::panic::take_hook());
    let hook_for_panic = std::sync::Arc::clone(&original_hook);
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), Show);
        (hook_for_panic)(info);
    }));
    let _panic_guard = PanicHookGuard { original_hook };

    let TerminalState {
        mut stdout,
        supports_enhancement,
    } = setup_terminal()?;

    let mut app = App::with_permissions(permissions).await?;
    handle_resume(&mut app, resume_option, supports_enhancement)?;

    let debug_events = std::env::var("ION_DEBUG_EVENTS").is_ok();

    // Track terminal size
    let (mut term_width, mut term_height) = terminal::size()?;

    reprint_loaded_session(&mut stdout, &mut app, term_width, term_height)?;

    // Main loop: prepare -> plan -> render
    loop {
        // 1. POLL — drain all pending events, collapsing burst resizes
        let had_event = if event::poll(std::time::Duration::from_millis(50))? {
            let mut last_resize: Option<(u16, u16)> = None;

            let evt = event::read()?;
            if debug_events {
                tracing::info!("Event: {:?}", evt);
            }
            match evt {
                event::Event::Resize(w, h) => last_resize = Some((w, h)),
                other => app.handle_event(other),
            }

            // Drain all immediately available events (zero-wait). Resize
            // debouncing is handled at the render level in prepare_frame.
            loop {
                if !event::poll(std::time::Duration::ZERO)? {
                    break;
                }
                let evt = event::read()?;
                if debug_events {
                    tracing::info!("Event: {:?}", evt);
                }
                match evt {
                    event::Event::Resize(w, h) => last_resize = Some((w, h)),
                    other => app.handle_event(other),
                }
            }

            // Apply only the final resize dimensions
            if let Some((w, h)) = last_resize {
                term_width = w;
                term_height = h;
                app.handle_event(event::Event::Resize(w, h));
            }

            true
        } else {
            // Some terminals don't emit Resize on tab switches; re-check size.
            if let Ok((w, h)) = terminal::size()
                && (w != term_width || h != term_height)
            {
                term_width = w;
                term_height = h;
                app.handle_event(event::Event::Resize(w, h));
            }
            false
        };

        let was_running = app.is_running;
        app.update();

        // 2. PREPARE
        let prep = prepare_frame(&mut app, term_width, term_height);

        // 3. PLAN
        let needs_render = prep.state_changed
            || had_event
            || app.is_running
            || was_running
            || !prep.chat_lines.is_empty();

        if !needs_render {
            if app.should_quit {
                break;
            }
            continue;
        }

        let layout = app.compute_layout(term_width, term_height);
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

        // 4. RENDER
        render_frame(
            &mut stdout,
            &mut app,
            prep.pre_ops,
            chat_insert,
            &layout,
            term_width,
            term_height,
        )?;

        if app.should_quit {
            break;
        }

        // Handle external editor request (Ctrl+G) -- suspends TUI
        if app.interaction.editor_requested {
            app.interaction.editor_requested = false;

            execute!(stdout, DisableBracketedPaste, DisableFocusChange)?;
            if supports_enhancement {
                execute!(stdout, PopKeyboardEnhancementFlags)?;
            }
            disable_raw_mode()?;
            execute!(stdout, Show)?;

            match open_editor(&app.input_buffer.get_content()) {
                Ok(Some(new_input)) => app.set_input_text(&new_input),
                Ok(None) => {}
                Err(e) => {
                    app.message_list.push_entry(MessageEntry::new(
                        Sender::System,
                        format!("Editor error: {e}"),
                    ));
                }
            }

            enable_raw_mode()?;
            execute!(stdout, EnableBracketedPaste, EnableFocusChange)?;
            if supports_enhancement {
                execute!(
                    stdout,
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    )
                )?;
            }
        }
    }

    cleanup_terminal(
        &app,
        &mut stdout,
        supports_enhancement,
        term_width,
        term_height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::render::PROGRESS_HEIGHT;
    use crate::tui::render::layout::{BodyLayout, Region, UiLayout};

    fn test_layout(top: u16, width: u16) -> UiLayout {
        let progress_height = PROGRESS_HEIGHT;
        let input_height = 3u16;
        let status_height = 1u16;
        let progress = Region {
            row: top,
            height: progress_height,
        };
        let input = Region {
            row: top + progress_height,
            height: input_height,
        };
        let status = Region {
            row: top + progress_height + input_height,
            height: status_height,
        };
        UiLayout {
            top,
            clear_from: top,
            body: BodyLayout::Input {
                popup: None,
                progress,
                input,
                status,
            },
            width,
        }
    }

    #[test]
    fn plan_at_row_fits() {
        let pos = ChatPosition::Tracking {
            next_row: 5,
            ui_drawn_at: None,
        };
        let lines = vec![StyledLine::empty(), StyledLine::empty()];
        let layout = test_layout(35, 80); // ui_height = 5
        let insert = plan_chat_insert(&pos, lines, &layout, 40);
        // 5 + 2 + 5 = 12 <= 40, so AtRow
        assert!(matches!(insert, ChatInsert::AtRow { start_row: 5, .. }));
    }

    #[test]
    fn plan_overflow() {
        let pos = ChatPosition::Tracking {
            next_row: 33,
            ui_drawn_at: None,
        };
        let lines = vec![
            StyledLine::empty(),
            StyledLine::empty(),
            StyledLine::empty(),
        ];
        let layout = test_layout(35, 80); // ui_height = 5
        let insert = plan_chat_insert(&pos, lines, &layout, 40);
        // 33 + 3 + 5 = 41 > 40, so Overflow
        assert!(matches!(insert, ChatInsert::Overflow { .. }));
    }

    #[test]
    fn plan_scroll_insert() {
        let pos = ChatPosition::Scrolling { ui_drawn_at: None };
        let lines = vec![StyledLine::empty(), StyledLine::empty()];
        let layout = test_layout(35, 80); // ui_height = 5
        let insert = plan_chat_insert(&pos, lines, &layout, 40);
        assert!(matches!(insert, ChatInsert::ScrollInsert { .. }));
    }

    #[test]
    fn plan_header_acts_like_tracking() {
        let pos = ChatPosition::Header { anchor: 3 };
        let lines = vec![StyledLine::empty()];
        let layout = test_layout(35, 80); // ui_height = 5
        let insert = plan_chat_insert(&pos, lines, &layout, 40);
        // 3 + 1 + 5 = 9 <= 40, so AtRow
        assert!(matches!(insert, ChatInsert::AtRow { start_row: 3, .. }));
    }

    #[test]
    fn plan_empty_starts_tracking_until_overflow() {
        let pos = ChatPosition::Empty;
        let lines = vec![StyledLine::empty()];
        let layout = test_layout(35, 80);
        let insert = plan_chat_insert(&pos, lines, &layout, 40);
        assert!(matches!(insert, ChatInsert::AtRow { start_row: 0, .. }));
    }

    #[test]
    fn plan_empty_overflow_transitions_like_tracking() {
        let pos = ChatPosition::Empty;
        let lines = vec![StyledLine::empty(); 40];
        let layout = test_layout(35, 80); // ui_height = 5, available = 35
        let insert = plan_chat_insert(&pos, lines, &layout, 40);
        assert!(matches!(insert, ChatInsert::Overflow { old_ui_row: 0, .. }));
    }

    #[test]
    fn tracking_ui_overlap_detects_intrusion() {
        let pos = ChatPosition::Tracking {
            next_row: 20,
            ui_drawn_at: Some(20),
        };
        let layout = test_layout(18, 80);
        assert!(tracking_ui_overlap(&pos, &layout));
    }

    #[test]
    fn tracking_ui_overlap_ignores_non_intrusion() {
        let pos = ChatPosition::Tracking {
            next_row: 20,
            ui_drawn_at: Some(20),
        };
        let layout = test_layout(20, 80);
        assert!(!tracking_ui_overlap(&pos, &layout));

        let scrolling = ChatPosition::Scrolling {
            ui_drawn_at: Some(20),
        };
        let layout = test_layout(18, 80);
        assert!(!tracking_ui_overlap(&scrolling, &layout));
    }

    #[test]
    fn overlap_scroll_disabled_for_selector_mode() {
        assert!(!should_scroll_for_overlap(Mode::Selector, false, 2, true));
        assert!(!should_scroll_for_overlap(Mode::Selector, false, 0, true));
    }

    #[test]
    fn overlap_scroll_enabled_only_when_needed() {
        assert!(should_scroll_for_overlap(Mode::Input, false, 1, true));
        assert!(!should_scroll_for_overlap(Mode::Input, true, 1, true));
        assert!(!should_scroll_for_overlap(Mode::Input, false, 0, true));
        assert!(!should_scroll_for_overlap(Mode::Input, false, 1, false));
    }
}
