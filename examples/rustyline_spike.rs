//! Spike: Evaluate rustyline-async for ion TUI
//!
//! Run with: cargo run --example rustyline_spike
//!
//! Tests:
//! 1. Basic readline with history
//! 2. Multi-line input
//! 3. SharedWriter for concurrent output
//! 4. Async task writing while user types

use rustyline_async::{Readline, ReadlineError, ReadlineEvent, SharedWriter};
use std::io::Write;
use tokio::time::{interval, Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== rustyline-async spike ===");
    println!("Commands: quit, async (start background task), multi (test multiline)");
    println!();

    let (mut rl, mut writer) = Readline::new("> ".to_string())?;

    // Spawn a background task that writes periodically
    let writer_clone = writer.clone();
    let bg_handle = tokio::spawn(async move {
        background_writer(writer_clone).await;
    });

    loop {
        match rl.readline().await {
            Ok(ReadlineEvent::Line(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                // Add to history
                rl.add_history_entry(line.to_string());

                match line {
                    "quit" | "exit" => {
                        writeln!(writer, "Goodbye!")?;
                        break;
                    }
                    "async" => {
                        writeln!(writer, "[Starting async output test - type while messages appear]")?;
                        let w = writer.clone();
                        tokio::spawn(async move {
                            async_output_test(w).await;
                        });
                    }
                    "multi" => {
                        writeln!(writer, "[Multi-line test: type multiple lines, empty line to finish]")?;
                        // Note: rustyline-async handles multi-line natively
                        // This is just demonstrating the API
                    }
                    _ => {
                        writeln!(writer, "You typed: {}", line)?;
                    }
                }
            }
            Ok(ReadlineEvent::Eof) => {
                writeln!(writer, "[EOF received - Ctrl+D]")?;
                break;
            }
            Ok(ReadlineEvent::Interrupted) => {
                writeln!(writer, "[Interrupted - Ctrl+C]")?;
                // Continue instead of exit to test behavior
            }
            Err(ReadlineError::Closed) => {
                eprintln!("[Readline closed]");
                break;
            }
            Err(e) => {
                eprintln!("Error: {:?}", e);
                break;
            }
        }
    }

    // Clean up
    bg_handle.abort();
    rl.flush()?;

    println!("\n=== Spike complete ===");
    Ok(())
}

/// Background task that periodically writes status updates
async fn background_writer(mut writer: SharedWriter) {
    let mut tick = interval(Duration::from_secs(10));
    let mut count = 0;

    loop {
        tick.tick().await;
        count += 1;
        let _ = writeln!(writer, "[Background tick #{}]", count);
    }
}

/// Test async output while user is typing
async fn async_output_test(mut writer: SharedWriter) {
    for i in 1..=5 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = writeln!(writer, "[Async message {} of 5 - keep typing!]", i);
    }
    let _ = writeln!(writer, "[Async test complete]");
}
