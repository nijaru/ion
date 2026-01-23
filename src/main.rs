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
        event::{self, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags},
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

    // Enable keyboard enhancement for Shift+Enter detection (Kitty protocol)
    let supports_enhancement = supports_keyboard_enhancement().unwrap_or(false);
    if supports_enhancement {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    let backend = CrosstermBackend::new(stdout);
    let (_, mut terminal_height) = crossterm::terminal::size()?;

    // Create app with permission settings
    let mut app = App::with_permissions(permissions).await?;

    // Full-height viewport - created once, only recreated on actual terminal resize.
    // UI is rendered at the bottom of the viewport; empty space above absorbs size changes.
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(terminal_height),
        },
    )?;
    let mut last_terminal_height = terminal_height;

    // Main loop
    loop {
        if event::poll(std::time::Duration::from_millis(50))? {
            match event::read()? {
                event::Event::Key(key) => {
                    app.handle_event(event::Event::Key(key));
                }
                event::Event::Resize(_, height) => {
                    terminal_height = height;
                }
                _ => {}
            }
        }

        app.update();

        // Only recreate terminal on ACTUAL terminal resize (not UI size changes)
        if terminal_height != last_terminal_height {
            // Clear before recreating to avoid artifacts
            terminal.draw(|frame| {
                frame.render_widget(ratatui::widgets::Clear, frame.area());
            })?;
            terminal = Terminal::with_options(
                CrosstermBackend::new(io::stdout()),
                TerminalOptions {
                    viewport: Viewport::Inline(terminal_height),
                },
            )?;
            last_terminal_height = terminal_height;
        }

        let width = terminal.size()?.width;
        let chat_lines = app.take_chat_inserts(width);
        if !chat_lines.is_empty() {
            let wrap_width = width.saturating_sub(2);
            if wrap_width > 0 {
                let paragraph = Paragraph::new(chat_lines.clone()).wrap(Wrap { trim: true });
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
            if supports_enhancement {
                execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
            }
            disable_raw_mode()?;
            terminal.show_cursor()?;

            // Open editor and get result
            if let Some(new_input) = open_editor(&app.input_buffer.get_content())? {
                app.set_input_text(&new_input);
            }

            // Re-enter TUI mode
            enable_raw_mode()?;
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
    if supports_enhancement {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    disable_raw_mode()?;
    terminal.show_cursor()?;

    Ok(())
}

/// Count the number of lines after wrapping text to a given width.
fn count_wrapped_lines(lines: &[ratatui::prelude::Line], width: usize) -> usize {
    if width == 0 {
        return lines.len();
    }

    let mut count = 0;
    for line in lines {
        let line_width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
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
