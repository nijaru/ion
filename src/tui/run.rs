//! TUI main loop and terminal management.
//!
//! This module contains the entry point for the TUI, terminal setup/cleanup,
//! and the main event/render loop.

use crate::cli::PermissionSettings;
use crate::tui::App;
use crate::tui::message_list::{MessageEntry, Sender};
use crate::tui::render_state::ChatPosition;
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
    execute!(stdout, MoveTo(0, layout.top), Clear(ClearType::FromCursorDown))?;
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
        execute!(
            stdout,
            crossterm::style::SetAttribute(crossterm::style::Attribute::Dim),
            crossterm::style::Print(format!("Session: {}", app.session.id)),
            crossterm::style::SetAttribute(crossterm::style::Attribute::Reset),
        )?;
        println!();
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
    let (cmd, args) = parts.split_first().ok_or_else(|| anyhow::anyhow!("Empty editor command"))?;
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

/// Main entry point for the TUI.
///
/// This function sets up the terminal, creates the App, handles resume options,
/// and runs the main event/render loop until the user quits.
#[allow(clippy::too_many_lines)]
pub async fn run(
    permissions: PermissionSettings,
    resume_option: ResumeOption,
) -> Result<()> {
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

    if !app.message_list.entries.is_empty() {
        // Push pre-ion terminal content to scrollback and start from row 0.
        // Wrap in synchronized update so the scroll + reprint is atomic.
        execute!(stdout, BeginSynchronizedUpdate)?;
        execute!(
            stdout,
            crossterm::terminal::ScrollUp(term_height),
            MoveTo(0, 0)
        )?;
        let line_count = app.reprint_chat_scrollback(&mut stdout, term_width)?;
        let layout = app.compute_layout(term_width, term_height);
        let ui_height = layout.height();
        let excess = app.render_state.position_after_reprint(line_count, term_height, ui_height);
        if excess > 0 {
            execute!(stdout, crossterm::terminal::ScrollUp(excess))?;
        }
        execute!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;
    }

    // Main loop
    loop {
        let had_event = if event::poll(std::time::Duration::from_millis(50))? {
            let evt = event::read()?;

            if debug_events {
                tracing::info!("Event: {:?}", evt);
            }

            if let event::Event::Resize(w, h) = evt {
                term_width = w;
                term_height = h;
            }
            app.handle_event(evt);
            true
        } else {
            false
        };

        // Some terminals don't emit Resize on tab switches; re-check size each frame.
        if let Ok((w, h)) = terminal::size()
            && (w != term_width || h != term_height)
        {
            term_width = w;
            term_height = h;
            app.handle_event(event::Event::Resize(w, h));
        }

        let was_running = app.is_running;
        app.update();

        let mut frame_changed = false;

        if app.render_state.needs_initial_render {
            app.render_state.needs_initial_render = false;
            frame_changed = true;
        }

        // Handle /clear: push visible content to scrollback, then blank viewport.
        // Unlike Clear(All), ScrollUp preserves content in terminal scrollback
        // so the user can scroll up to see their previous conversation.
        if app.render_state.needs_screen_clear {
            app.render_state.needs_screen_clear = false;
            execute!(
                stdout,
                BeginSynchronizedUpdate,
                crossterm::terminal::ScrollUp(term_height),
                MoveTo(0, 0),
                EndSynchronizedUpdate
            )?;
            stdout.flush()?;
            frame_changed = true;
        }

        // Handle resize: push viewport to scrollback, then reprint chat at new width.
        // ScrollUp preserves pre-ion terminal content in scrollback (unlike Clear(All)
        // which would erase it). Old chat at wrong width ends up in scrollback too --
        // acceptable trade-off vs losing the user's terminal history.
        if app.render_state.needs_reflow {
            app.render_state.needs_reflow = false;
            execute!(stdout, BeginSynchronizedUpdate)?;
            execute!(
                stdout,
                crossterm::terminal::ScrollUp(term_height),
                MoveTo(0, 0)
            )?;
            if !app.message_list.entries.is_empty() {
                let all_lines = app.build_chat_lines(term_width);
                let layout = app.compute_layout(term_width, term_height);
                let ui_height = layout.height();

                for line in &all_lines {
                    line.writeln(&mut stdout)?;
                }

                let excess = app.render_state.position_after_reprint(all_lines.len(), term_height, ui_height);
                if excess > 0 {
                    execute!(stdout, crossterm::terminal::ScrollUp(excess))?;
                }

                let mut end = app.message_list.entries.len();
                if app.is_running
                    && app
                        .message_list
                        .entries
                        .last()
                        .is_some_and(|e| e.sender == Sender::Agent)
                {
                    end = end.saturating_sub(1);
                }
                app.render_state.mark_reflow_complete(end);
            } else {
                app.render_state.position = ChatPosition::Empty;
            }
            execute!(stdout, EndSynchronizedUpdate)?;
            stdout.flush()?;
            frame_changed = true;
        }

        // Handle selector close: clear the selector area
        if app.render_state.needs_selector_clear {
            app.render_state.needs_selector_clear = false;
            let sel_layout = app.compute_layout(term_width, term_height);
            execute!(
                stdout,
                MoveTo(0, sel_layout.top),
                Clear(ClearType::FromCursorDown)
            )?;
            stdout.flush()?;
            frame_changed = true;
        }

        if !app.render_state.position.header_inserted() {
            let header_lines = app.take_startup_header_lines();
            if !header_lines.is_empty() {
                for line in &header_lines {
                    line.writeln(&mut stdout)?;
                }
                if let Ok((_x, y)) = crossterm::cursor::position() {
                    app.render_state.position = ChatPosition::Header { anchor: y };
                }
                frame_changed = true;
            }
        }

        // Print any new chat content using insert_before pattern
        let chat_lines = app.take_chat_inserts(term_width);

        // If this is the first message, clear startup UI BEFORE sync update
        if !chat_lines.is_empty()
            && let ChatPosition::Header { anchor } = app.render_state.position
        {
            execute!(stdout, MoveTo(0, anchor), Clear(ClearType::FromCursorDown))?;
            stdout.flush()?;
        }

        // Only render when something changed: event, agent running/stopped,
        // new chat content, or a flag was processed above.
        let needs_render = frame_changed
            || had_event
            || app.is_running
            || was_running
            || !chat_lines.is_empty();

        if !needs_render {
            if app.should_quit {
                break;
            }
            continue;
        }

        // Begin synchronized output (prevents flicker)
        execute!(stdout, BeginSynchronizedUpdate)?;

        if !chat_lines.is_empty() {
            let layout = app.compute_layout(term_width, term_height);
            let ui_height = layout.height();

            #[allow(clippy::cast_possible_truncation)] // Chat lines fit in terminal u16 height
            let line_count = chat_lines.len() as u16;

            // Row-tracking mode: print at tracked row if content fits
            match app.render_state.position {
                ChatPosition::Tracking { next_row, .. } | ChatPosition::Header { anchor: next_row } => {
                    let space_needed = next_row
                        .saturating_add(line_count)
                        .saturating_add(ui_height);
                    if space_needed <= term_height {
                        // Content fits: print at current row, advance position
                        for (i, line) in chat_lines.iter().enumerate() {
                            #[allow(clippy::cast_possible_truncation)]
                            execute!(
                                stdout,
                                MoveTo(0, next_row.saturating_add(i as u16)),
                                Clear(ClearType::CurrentLine)
                            )?;
                            line.writeln(&mut stdout)?;
                        }
                        let new_next_row = next_row.saturating_add(line_count);
                        app.render_state.position = ChatPosition::Tracking {
                            next_row: new_next_row,
                            ui_drawn_at: Some(new_next_row),
                        };
                    } else {
                        // Overflow: transition to scroll mode
                        let content_end = next_row.saturating_add(line_count);
                        let ui_start = term_height.saturating_sub(ui_height);
                        let scroll_amount = content_end.saturating_sub(ui_start);

                        // Clear old UI at next_row (where it was drawn in row-tracking mode)
                        // before scrolling, so borders don't get pushed into scrollback
                        execute!(stdout, MoveTo(0, next_row), Clear(ClearType::FromCursorDown))?;
                        execute!(stdout, crossterm::terminal::ScrollUp(scroll_amount))?;

                        // Print at top of the scrolled area
                        let print_row = ui_start.saturating_sub(line_count);
                        for (i, line) in chat_lines.iter().enumerate() {
                            #[allow(clippy::cast_possible_truncation)]
                            execute!(
                                stdout,
                                MoveTo(0, print_row.saturating_add(i as u16)),
                                Clear(ClearType::CurrentLine)
                            )?;
                            line.writeln(&mut stdout)?;
                        }
                        app.render_state.position = ChatPosition::Scrolling { ui_drawn_at: None };
                    }
                }
                ChatPosition::Scrolling { .. } | ChatPosition::Empty => {
                    // Scroll mode: existing behavior (content pushed into scrollback)
                    let ui_start = term_height.saturating_sub(ui_height);

                    // Clear old UI before scrolling so borders don't get pushed into scrollback
                    execute!(stdout, MoveTo(0, ui_start), Clear(ClearType::FromCursorDown))?;

                    // Insert lines by scrolling up (now pushes blank lines, not old UI)
                    execute!(stdout, crossterm::terminal::ScrollUp(line_count))?;

                    // Print at the newly created space (just above where UI will be)
                    let mut row = ui_start.saturating_sub(line_count);
                    for line in &chat_lines {
                        execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
                        line.writeln(&mut stdout)?;
                        row = row.saturating_add(1);
                    }
                }
            }
        }

        // Compute layout once per frame, render the bottom UI area
        let layout = app.compute_layout(term_width, term_height);
        app.draw_direct(&mut stdout, &layout)?;

        // End synchronized output
        execute!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;

        if app.should_quit {
            break;
        }

        // Handle external editor request (Ctrl+G)
        if app.interaction.editor_requested {
            app.interaction.editor_requested = false;

            // Temporarily restore terminal for editor
            execute!(stdout, DisableBracketedPaste, DisableFocusChange)?;
            if supports_enhancement {
                execute!(stdout, PopKeyboardEnhancementFlags)?;
            }
            disable_raw_mode()?;
            execute!(stdout, Show)?;

            // Open editor and get result (handle errors gracefully)
            match open_editor(&app.input_buffer.get_content()) {
                Ok(Some(new_input)) => app.set_input_text(&new_input),
                Ok(None) => {} // No changes or editor cancelled
                Err(e) => {
                    // Show error in TUI chat instead of stderr
                    app.message_list.push_entry(MessageEntry::new(
                        Sender::System,
                        format!("Editor error: {e}"),
                    ));
                }
            }

            // Re-enter TUI mode
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
