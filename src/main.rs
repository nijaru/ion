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
        event, execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    };
    use ion::tui::App;
    use ratatui::prelude::*;
    use std::io;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
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
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
