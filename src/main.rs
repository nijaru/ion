use clap::Parser;
use ion::cli::{Cli, Commands, PermissionSettings};
use ion::config::Config;
use std::process::ExitCode;

/// Resume option for TUI mode.
#[derive(Debug, Clone)]
enum ResumeOption {
    None,
    Latest,
    ById(String),
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run(args)) => {
            // One-shot CLI mode
            ion::cli::run(args, cli.auto_approve).await
        }
        None => {
            // Load config for permission defaults
            let config = match Config::load() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error loading config: {}", e);
                    return ExitCode::FAILURE;
                }
            };

            // Resolve permission settings from CLI flags and config
            let permissions = cli.resolve_permissions(&config);

            // Determine resume option from CLI flags
            let resume_option = if let Some(ref id) = cli.session_id {
                ResumeOption::ById(id.clone())
            } else if cli.resume {
                ResumeOption::Latest
            } else {
                ResumeOption::None
            };

            // Interactive TUI mode
            match run_tui(permissions, resume_option).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
    }
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
            self, disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement,
            BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate,
        },
    };
    use ion::tui::App;
    use std::io::{self, Write};

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();

    // Enable bracketed paste mode (prevents terminal from treating paste as commands)
    execute!(stdout, EnableBracketedPaste)?;

    // Enable keyboard enhancement for Shift+Enter detection (Kitty protocol)
    let supports_enhancement = supports_keyboard_enhancement().unwrap_or(false);
    if supports_enhancement {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    // Create app with permission settings
    let mut app = App::with_permissions(permissions).await?;

    // Handle resume option
    match resume_option {
        ResumeOption::None => {}
        ResumeOption::Latest => {
            // Load most recent session
            if let Ok(sessions) = app.store.list_recent(1)
                && let Some(session) = sessions.first()
                    && let Err(e) = app.load_session(&session.id) {
                        eprintln!("Warning: Failed to load session: {}", e);
                    }
        }
        ResumeOption::ById(id) => {
            if let Err(e) = app.load_session(&id) {
                // Restore terminal before printing error
                let _ = execute!(io::stdout(), DisableBracketedPaste);
                let _ = disable_raw_mode();
                eprintln!("Error: Session '{}' not found: {}", id, e);
                return Err(e.into());
            }
        }
    }

    // Check for debug mode via environment variable
    let debug_events = std::env::var("ION_DEBUG_EVENTS").is_ok();

    // Track terminal size
    let (mut term_width, mut term_height) = terminal::size()?;

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
                    app.handle_event(event::Event::Resize(w, h));
                    // Clear scrollback + screen + home cursor for full redraw
                    // \x1b[3J = clear scrollback, \x1b[2J = clear screen, \x1b[H = home cursor
                    print!("\x1b[3J\x1b[2J\x1b[H");
                    let _ = std::io::stdout().flush();
                }
                _ => {}
            }
        }

        // Some terminals don't emit Resize on tab switches; re-check size each frame.
        if let Ok((w, h)) = terminal::size() {
            if w != term_width || h != term_height {
                term_width = w;
                term_height = h;
                app.handle_event(event::Event::Resize(w, h));
                print!("\x1b[3J\x1b[2J\x1b[H");
                let _ = std::io::stdout().flush();
            }
        }

        app.update();

        // Begin synchronized output (prevents flicker)
        execute!(stdout, BeginSynchronizedUpdate)?;

        // Print any new chat content using insert_before pattern
        let chat_lines = app.take_chat_inserts(term_width);
        if !chat_lines.is_empty() {
            let ui_height = app.calculate_ui_height(term_width, term_height);
            let line_count = chat_lines.len() as u16;

            // Move to where UI starts, scroll up to make room, then print
            let ui_start = term_height.saturating_sub(ui_height);
            execute!(stdout, MoveTo(0, ui_start))?;

            // Insert lines by scrolling up (pushes existing content into scrollback)
            execute!(stdout, crossterm::terminal::ScrollUp(line_count))?;

            // Print at the newly created space (just above where UI will be)
            execute!(stdout, MoveTo(0, ui_start.saturating_sub(line_count)))?;
            for line in &chat_lines {
                line.println()?;
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
            execute!(stdout, DisableBracketedPaste)?;
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
                    app.message_list.push_entry(
                        ion::tui::message_list::MessageEntry::new(
                            ion::tui::message_list::Sender::System,
                            format!("Editor error: {}", e),
                        ),
                    );
                }
            }

            // Re-enter TUI mode
            enable_raw_mode()?;
            execute!(stdout, EnableBracketedPaste)?;
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

    // Clear bottom UI area before exit
    let ui_height = app.calculate_ui_height(term_width, term_height);
    let ui_start = term_height.saturating_sub(ui_height);
    execute!(stdout, MoveTo(0, ui_start), Clear(ClearType::FromCursorDown))?;

    // Restore terminal
    execute!(stdout, DisableBracketedPaste)?;
    if supports_enhancement {
        execute!(stdout, PopKeyboardEnhancementFlags)?;
    }
    disable_raw_mode()?;
    execute!(stdout, Show)?;

    Ok(())
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
