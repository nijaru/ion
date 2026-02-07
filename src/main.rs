use clap::Parser;
use ion::cli::{Cli, Commands};
use ion::config::Config;
use ion::tui::ResumeOption;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run(args)) => {
            // One-shot CLI mode
            ion::cli::run(args).await
        }
        Some(Commands::Login(args)) => {
            // OAuth login
            ion::cli::login(args).await
        }
        Some(Commands::Logout(args)) => {
            // OAuth logout
            ion::cli::logout(args)
        }
        Some(Commands::Config(args)) => {
            // View or modify configuration
            ion::cli::config(args)
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
            match ion::tui::run(permissions, resume_option).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("Error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}
