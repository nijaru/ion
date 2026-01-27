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
        event::{
            self, DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
            PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
        },
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement},
    };
    use ion::tui::App;
    use ratatui::{TerminalOptions, Viewport, prelude::*};
    use ratatui::widgets::{Paragraph, Wrap};
    use std::io;

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

    let backend = CrosstermBackend::new(stdout);

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

    // Fixed-size viewport for UI only. Chat content appears above via insert_before.
    // Using a fixed size (not full terminal height) ensures inserted content is visible.
    // Never recreated to preserve scrollback.
    // 15 lines allows: progress (2) + input (up to 12) + status (1)
    const UI_VIEWPORT_HEIGHT: u16 = 15;
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(UI_VIEWPORT_HEIGHT),
        },
    )?;

    // Main loop
    loop {
        if event::poll(std::time::Duration::from_millis(50))? {
            match event::read()? {
                event::Event::Key(key) => {
                    app.handle_event(event::Event::Key(key));
                }
                event::Event::Resize(_, _) => {
                    // Inline viewports auto-resize during draw(), no action needed.
                    // Recreating the terminal would break scrollback preservation.
                }
                _ => {}
            }
        }

        app.update();

        let width = terminal.size()?.width;
        let chat_lines = app.take_chat_inserts(width);
        if !chat_lines.is_empty() {
            let wrap_width = width.saturating_sub(2);
            if wrap_width > 0 {
                let paragraph = Paragraph::new(chat_lines.clone()).wrap(Wrap { trim: false });
                // Calculate height by counting wrapped lines
                let height = count_wrapped_lines(&chat_lines, wrap_width as usize);
                if height > 0 {
                    let height = u16::try_from(height).unwrap_or(u16::MAX);
                    terminal.insert_before(height, |buf| {
                        let area = Rect::new(1, 0, wrap_width, height);
                        paragraph.render(area, buf);
                    })?;
                }
            }
        }

        terminal.draw(|f| app.draw(f))?;

        if app.should_quit {
            break;
        }

        // Handle external editor request (Ctrl+G)
        if app.editor_requested {
            app.editor_requested = false;

            // Temporarily restore terminal for editor
            execute!(terminal.backend_mut(), DisableBracketedPaste)?;
            if supports_enhancement {
                execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
            }
            disable_raw_mode()?;
            terminal.show_cursor()?;

            // Open editor and get result (handle errors gracefully)
            match open_editor(&app.input_buffer.get_content()) {
                Ok(Some(new_input)) => app.set_input_text(&new_input),
                Ok(None) => {} // No changes or editor cancelled
                Err(e) => {
                    // Show error in TUI chat instead of stderr (which may be hidden in raw mode)
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
            execute!(terminal.backend_mut(), EnableBracketedPaste)?;
            if supports_enhancement {
                execute!(
                    terminal.backend_mut(),
                    PushKeyboardEnhancementFlags(
                        KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    )
                )?;
            }
        }
    }

    // Clear viewport before exit to prevent input box from being left in scrollback
    terminal.draw(|frame| {
        frame.render_widget(ratatui::widgets::Clear, frame.area());
    })?;

    // Restore terminal
    execute!(terminal.backend_mut(), DisableBracketedPaste)?;
    if supports_enhancement {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    disable_raw_mode()?;
    terminal.show_cursor()?;

    Ok(())
}

/// Count the number of lines after wrapping text to a given width.
fn count_wrapped_lines(lines: &[ratatui::prelude::Line], width: usize) -> usize {
    use unicode_width::UnicodeWidthStr;

    if width == 0 {
        return lines.len();
    }

    let mut count = 0;
    for line in lines {
        let line_width: usize = line.spans.iter().map(|s| s.content.width()).sum();
        if line_width == 0 {
            count += 1;
        } else {
            count += line_width.div_ceil(width);
        }
    }
    count
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

    // Open editor
    let status = Command::new(&editor).arg(temp.path()).status()?;

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
