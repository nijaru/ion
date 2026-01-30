use clap::Parser;
use ion::cli::{Cli, Commands, PermissionSettings};
use ion::config::Config;
use ion::tui::Sender;
use std::process::ExitCode;

/// Resume option for TUI mode.
#[derive(Debug, Clone)]
enum ResumeOption {
    None,
    Latest,
    ById(String),
    Selector,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run(args)) => {
            // One-shot CLI mode
            ion::cli::run(args, cli.auto_approve).await
        }
        Some(Commands::Login(args)) => {
            // OAuth login
            ion::cli::login(args).await
        }
        Some(Commands::Logout(args)) => {
            // OAuth logout
            ion::cli::logout(args)
        }
        None => {
            // Load config for permission defaults
            let config = match Config::load() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error loading config: {e}");
                    return ExitCode::FAILURE;
                }
            };

            // Resolve permission settings from CLI flags and config
            let permissions = cli.resolve_permissions(&config);

            // Determine resume option from CLI flags
            let resume_option = if cli.continue_session {
                ResumeOption::Latest
            } else if let Some(ref value) = cli.resume {
                if value == "__SELECT__" {
                    ResumeOption::Selector
                } else {
                    ResumeOption::ById(value.clone())
                }
            } else {
                ResumeOption::None
            };

            // Interactive TUI mode
            match run_tui(permissions, resume_option).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("Error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// Terminal state returned from setup.
struct TerminalState {
    stdout: std::io::Stdout,
    supports_enhancement: bool,
}

/// Setup terminal for TUI mode (raw mode, bracketed paste, keyboard enhancement).
fn setup_terminal() -> Result<TerminalState, Box<dyn std::error::Error>> {
    use crossterm::{
        event::{
            EnableBracketedPaste, EnableFocusChange, KeyboardEnhancementFlags,
            PushKeyboardEnhancementFlags,
        },
        execute,
        terminal::{enable_raw_mode, supports_keyboard_enhancement},
    };
    use std::io;

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
/// Returns Err if session load fails fatally (ById with invalid id).
fn handle_resume(
    app: &mut ion::tui::App,
    resume_option: ResumeOption,
) -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::{
        event::{DisableBracketedPaste, DisableFocusChange},
        execute,
        terminal::disable_raw_mode,
    };
    use ion::tui::message_list::{MessageEntry, Sender};
    use std::io;

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
                let _ = execute!(io::stdout(), DisableBracketedPaste, DisableFocusChange);
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
    app: &ion::tui::App,
    stdout: &mut std::io::Stdout,
    supports_enhancement: bool,
    term_width: u16,
    term_height: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::{
        cursor::{MoveTo, Show},
        event::{DisableBracketedPaste, DisableFocusChange, PopKeyboardEnhancementFlags},
        execute,
        terminal::{disable_raw_mode, Clear, ClearType},
    };

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

async fn run_tui(
    permissions: PermissionSettings,
    resume_option: ResumeOption,
) -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::{
        cursor::{MoveTo, Show},
        event::{
            self, DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
            PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
        },
        execute,
        terminal::{
            self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate,
            disable_raw_mode, enable_raw_mode,
        },
    };
    use ion::tui::App;
    use std::io::Write;

    let TerminalState {
        mut stdout,
        supports_enhancement,
    } = setup_terminal()?;

    let mut app = App::with_permissions(permissions).await?;
    handle_resume(&mut app, resume_option)?;

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
                        let _ = std::io::stdout().flush();
                        app.reprint_chat_scrollback(&mut stdout, term_width)?;
                        stdout.flush()?;
                    }
                }
                _ => {}
            }
        }

        // Some terminals don't emit Resize on tab switches; re-check size each frame.
        if let Ok((w, h)) = terminal::size()
            && (w != term_width || h != term_height) {
                term_width = w;
                term_height = h;
                let has_chat = !app.message_list.entries.is_empty();
                app.handle_event(event::Event::Resize(w, h));
                if has_chat {
                    print!("\x1b[3J\x1b[2J\x1b[H");
                    let _ = std::io::stdout().flush();
                    app.reprint_chat_scrollback(&mut stdout, term_width)?;
                    stdout.flush()?;
                }
            }

        app.update();

        // Handle full repaint request (e.g., after exiting fullscreen selector)
        if app.render_state.needs_full_repaint {
            app.render_state.needs_full_repaint = false;
            // Clear screen (always) and reprint chat (if any)
            print!("\x1b[3J\x1b[2J\x1b[H");
            let _ = std::io::stdout().flush();
            if !app.message_list.entries.is_empty() {
                app.reprint_chat_scrollback(&mut stdout, term_width)?;
            }
            // Re-insert header if no chat yet
            if app.message_list.entries.is_empty() {
                app.set_header_inserted(false);
            }
            stdout.flush()?;
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
            && let Some(anchor) = app.take_startup_ui_anchor() {
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
                let space_needed = chat_row.saturating_add(line_count).saturating_add(ui_height);
                if space_needed <= term_height {
                    // Content fits: print at current row, advance chat_row
                    for (i, line) in chat_lines.iter().enumerate() {
                        execute!(stdout, MoveTo(0, chat_row.saturating_add(i as u16)))?;
                        line.println()?;
                    }
                    app.render_state.chat_row = Some(chat_row.saturating_add(line_count));
                } else {
                    // Overflow: transition to scroll mode
                    let content_end = chat_row.saturating_add(line_count);
                    let ui_start = term_height.saturating_sub(ui_height);
                    let scroll_amount = content_end.saturating_sub(ui_start);

                    execute!(stdout, MoveTo(0, ui_start))?;
                    execute!(stdout, crossterm::terminal::ScrollUp(scroll_amount))?;

                    // Print at top of the scrolled area
                    let print_row = ui_start.saturating_sub(line_count);
                    for (i, line) in chat_lines.iter().enumerate() {
                        execute!(stdout, MoveTo(0, print_row.saturating_add(i as u16)), Clear(ClearType::CurrentLine))?;
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

                // Move to where UI starts, scroll up to make room, then print
                execute!(stdout, MoveTo(0, ui_start))?;

                // Insert lines by scrolling up (pushes existing content into scrollback)
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
            execute!(stdout, DisableBracketedPaste, event::DisableFocusChange)?;
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
                    app.message_list
                        .push_entry(ion::tui::message_list::MessageEntry::new(
                            ion::tui::message_list::Sender::System,
                            format!("Editor error: {e}"),
                        ));
                }
            }

            // Re-enter TUI mode
            enable_raw_mode()?;
            execute!(stdout, EnableBracketedPaste, event::EnableFocusChange)?;
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

    cleanup_terminal(&app, &mut stdout, supports_enhancement, term_width, term_height)
}

/// Open text in external editor, returns edited content or None if unchanged/cancelled
fn open_editor(initial: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    use std::io::Write;
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
