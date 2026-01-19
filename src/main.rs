use clap::Parser;
use ion::cli::{Cli, Commands, PermissionSettings};
use ion::config::Config;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run(args)) => {
            // One-shot CLI mode
            ion::cli::run(args).await
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

            // Interactive TUI mode
            match run_tui(permissions).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    ExitCode::FAILURE
                }
            }
        }
    }
}

async fn run_tui(permissions: PermissionSettings) -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::{
        event::{
            self, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
            PushKeyboardEnhancementFlags,
        },
        execute,
        terminal::{
            EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
            supports_keyboard_enhancement,
        },
    };
    use ion::tui::App;
    use ratatui::prelude::*;
    use std::io;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();

    // Enable keyboard enhancement for Shift+Enter detection (Kitty protocol)
    let supports_enhancement = supports_keyboard_enhancement().unwrap_or(false);
    if supports_enhancement {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app with permission settings
    let mut app = App::with_permissions(permissions).await;

    // Main loop
    loop {
        terminal.draw(|f| app.draw(f))?;

        if event::poll(std::time::Duration::from_millis(50))?
            && let event::Event::Key(key) = event::read()?
        {
            app.handle_event(event::Event::Key(key));
        }

        app.update();

        if app.should_quit {
            break;
        }

        // Handle external editor request (Ctrl+G)
        if app.editor_requested {
            app.editor_requested = false;

            // Temporarily restore terminal for editor
            if supports_enhancement {
                execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
            }
            disable_raw_mode()?;
            execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
            terminal.show_cursor()?;

            // Open editor and get result
            if let Some(new_input) = open_editor(&app.input)? {
                app.input = new_input;
                app.cursor_pos = app.input.len();
            }

            // Re-enter TUI mode
            enable_raw_mode()?;
            if supports_enhancement {
                execute!(
                    terminal.backend_mut(),
                    PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
                )?;
            }
            execute!(terminal.backend_mut(), EnterAlternateScreen)?;
            terminal.hide_cursor()?;
        }
    }

    // Restore terminal
    if supports_enhancement {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

/// Open text in external editor, returns edited content or None if unchanged/cancelled
fn open_editor(initial: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    use std::io::Write;
    use std::process::Command;

    // Get editor from environment
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());

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
