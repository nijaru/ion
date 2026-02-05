//! TUI main loop and terminal management.
//!
//! This module contains the entry point for the TUI, terminal setup/cleanup,
//! and the main event/render loop.

use crate::cli::PermissionSettings;
use crate::tui::App;
use crate::tui::message_list::{MessageEntry, Sender};
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
fn setup_terminal() -> Result<TerminalState, Box<dyn std::error::Error>> {
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
) -> Result<(), Box<dyn std::error::Error>> {
    match resume_option {
        ResumeOption::None => {}
        ResumeOption::Latest => match app.store.list_recent(1) {
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
                        "No recent sessions found.".to_string(),
                    ));
                }
            }
            Err(e) => {
                app.message_list.push_entry(MessageEntry::new(
                    Sender::System,
                    format!("Error: Failed to list sessions: {e}"),
                ));
            }
        },
        ResumeOption::ById(id) => {
            if let Err(e) = app.load_session(&id) {
                let mut stdout = io::stdout();
                let _ = execute!(stdout, DisableBracketedPaste, DisableFocusChange);
                if supports_enhancement {
                    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
                }
                let _ = disable_raw_mode();
                eprintln!("Error: Session '{id}' not found: {e}");
                return Err(e.into());
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
) -> Result<(), Box<dyn std::error::Error>> {
    // Clear UI area before exit
    let ui_height = app.calculate_ui_height(term_width, term_height);
    let ui_start = app.ui_start_row(term_height, ui_height);
    let ui_end = ui_start.saturating_add(ui_height).min(term_height);
    for row in ui_start..ui_end {
        execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    }
    // Position cursor at ui_start (just after chat content)
    execute!(stdout, MoveTo(0, ui_start))?;

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
fn open_editor(initial: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    use std::process::Command;

    // Get editor from environment (VISUAL for full-screen, EDITOR as fallback)
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| "No editor configured. Set VISUAL or EDITOR environment variable.\nExample: export VISUAL=nano")?;

    // Create temp file with initial content
    let mut temp = tempfile::NamedTempFile::with_suffix(".md")?;
    temp.write_all(initial.as_bytes())?;
    temp.flush()?;

    // Open editor - split command and args (handles "code --wait", "nvim -u NONE", etc.)
    let parts: Vec<&str> = editor.split_whitespace().collect();
    let (cmd, args) = parts.split_first().ok_or("Empty editor command")?;
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
struct PanicHookGuard;

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        let _ = std::panic::take_hook();
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
) -> Result<(), Box<dyn std::error::Error>> {
    // Set panic hook to restore terminal on panic (guard ensures cleanup on all exit paths)
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), Show);
        original_hook(info);
    }));
    let _panic_guard = PanicHookGuard;

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
        app.reprint_chat_scrollback(&mut stdout, term_width)?;
        stdout.flush()?;
    }

    // Main loop
    loop {
        if event::poll(std::time::Duration::from_millis(50))? {
            let evt = event::read()?;

            // Debug: log raw events when ION_DEBUG_EVENTS is set
            if debug_events {
                tracing::info!("Event: {:?}", evt);
            }

            match evt {
                event::Event::Key(key) => {
                    app.handle_event(event::Event::Key(key));
                }
                event::Event::Paste(text) => {
                    app.handle_event(event::Event::Paste(text));
                }
                event::Event::Resize(w, h) => {
                    term_width = w;
                    term_height = h;
                    let has_chat = !app.message_list.entries.is_empty();
                    app.handle_event(event::Event::Resize(w, h));
                    if has_chat {
                        // Clear scrollback + screen + home cursor for full reflow
                        // \x1b[3J = clear scrollback, \x1b[2J = clear screen, \x1b[H = home cursor
                        print!("\x1b[3J\x1b[2J\x1b[H");
                        let _ = io::stdout().flush();
                        app.reprint_chat_scrollback(&mut stdout, term_width)?;
                        stdout.flush()?;
                    }
                }
                _ => {}
            }
        }

        // Some terminals don't emit Resize on tab switches; re-check size each frame.
        if let Ok((w, h)) = terminal::size()
            && (w != term_width || h != term_height)
        {
            term_width = w;
            term_height = h;
            let has_chat = !app.message_list.entries.is_empty();
            app.handle_event(event::Event::Resize(w, h));
            if has_chat {
                print!("\x1b[3J\x1b[2J\x1b[H");
                let _ = io::stdout().flush();
                app.reprint_chat_scrollback(&mut stdout, term_width)?;
                stdout.flush()?;
            }
        }

        app.update();

        // Handle full repaint request (e.g., after exiting fullscreen selector)
        if app.render_state.needs_full_repaint {
            app.render_state.needs_full_repaint = false;
            let clear_scrollback = app.render_state.clear_scrollback_on_repaint;
            // Clear screen (always); clear scrollback only when requested
            if clear_scrollback {
                print!("\x1b[3J\x1b[2J\x1b[H");
            } else {
                print!("\x1b[2J\x1b[H");
            }
            let _ = io::stdout().flush();
            if !app.message_list.entries.is_empty() {
                app.reprint_chat_scrollback(&mut stdout, term_width)?;
            }
            // Re-insert header if no chat yet
            if app.message_list.entries.is_empty() {
                app.set_header_inserted(false);
            }
            stdout.flush()?;
            // Reset to default behavior for future repaints
            app.render_state.clear_scrollback_on_repaint = true;
        }

        if !app.header_inserted() && app.message_list.entries.is_empty() {
            let header_lines = app.take_startup_header_lines();
            if !header_lines.is_empty() {
                for line in &header_lines {
                    line.println()?;
                }
                if let Ok((_x, y)) = crossterm::cursor::position() {
                    app.set_startup_ui_anchor(Some(y));
                    // Initialize row-tracking mode: chat will start at this row
                    app.render_state.chat_row = Some(y);
                }
            }
        }

        // Print any new chat content using insert_before pattern
        let chat_lines = app.take_chat_inserts(term_width);

        // If this is the first message, clear startup UI BEFORE sync update
        // This ensures the clear is fully processed before we start scrolling
        if !chat_lines.is_empty()
            && let Some(anchor) = app.take_startup_ui_anchor()
        {
            execute!(stdout, MoveTo(0, anchor), Clear(ClearType::FromCursorDown))?;
            stdout.flush()?;
        }

        // Begin synchronized output (prevents flicker)
        execute!(stdout, BeginSynchronizedUpdate)?;

        if !chat_lines.is_empty() {
            let ui_height = app.calculate_ui_height(term_width, term_height);

            #[allow(clippy::cast_possible_truncation)] // Chat lines fit in terminal u16 height
            let line_count = chat_lines.len() as u16;

            // Row-tracking mode: print at tracked row if content fits
            if let Some(chat_row) = app.render_state.chat_row {
                let space_needed = chat_row
                    .saturating_add(line_count)
                    .saturating_add(ui_height);
                if space_needed <= term_height {
                    // Content fits: print at current row, advance chat_row
                    for (i, line) in chat_lines.iter().enumerate() {
                        #[allow(clippy::cast_possible_truncation)]
                        execute!(
                            stdout,
                            MoveTo(0, chat_row.saturating_add(i as u16)),
                            Clear(ClearType::CurrentLine)
                        )?;
                        line.println()?;
                    }
                    app.render_state.chat_row = Some(chat_row.saturating_add(line_count));
                } else {
                    // Overflow: transition to scroll mode
                    let content_end = chat_row.saturating_add(line_count);
                    let ui_start = term_height.saturating_sub(ui_height);
                    let scroll_amount = content_end.saturating_sub(ui_start);

                    // Clear old UI before scrolling so borders don't get pushed into scrollback
                    execute!(stdout, MoveTo(0, ui_start), Clear(ClearType::FromCursorDown))?;
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
                        line.println()?;
                    }
                    // Transition to scroll mode - reset both chat_row and last_ui_start
                    // to prevent draw_direct from using stale row-tracking values
                    app.render_state.chat_row = None;
                    app.render_state.last_ui_start = None;
                }
            } else {
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
                    line.println()?;
                    row = row.saturating_add(1);
                }
            }
        }

        // Render the bottom UI area
        app.draw_direct(&mut stdout, term_width, term_height)?;

        // End synchronized output
        execute!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;

        if app.should_quit {
            break;
        }

        // Handle external editor request (Ctrl+G)
        if app.editor_requested {
            app.editor_requested = false;

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
