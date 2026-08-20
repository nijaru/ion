//! Ion CLI host. This binary owns process lifetime and frontend selection.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;
use ion_core::{
    PrintFrontend, Runtime, RuntimeError, ScriptedMessage, ScriptedProvider, ToolRegistry,
};

#[derive(Parser, Debug)]
#[command(
    name = "ion",
    version,
    about = "Ion terminal coding agent",
    disable_help_subcommand = true
)]
struct Cli {
    /// Run one prompt through print mode and exit.
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    print: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();
    let Some(prompt) = cli.print else {
        let _ = writeln!(
            io::stderr(),
            "interactive TUI is not implemented yet; use ion -p \"prompt\""
        );
        return ExitCode::from(2);
    };

    match run_print(prompt).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            ExitCode::FAILURE
        }
    }
}

async fn run_print(prompt: String) -> Result<(), RuntimeError> {
    let provider =
        ScriptedProvider::new(vec![ScriptedMessage::text(format!("scripted: {prompt}\n"))]);
    let tools = ToolRegistry::default();
    let runtime = Runtime::start(provider, tools);
    let handle = runtime.handle();
    let result = PrintFrontend::new(io::stdout()).run(&handle, prompt).await;
    let shutdown = handle.shutdown().await;
    let join = runtime.join().await;
    result?;
    shutdown?;
    join
}
