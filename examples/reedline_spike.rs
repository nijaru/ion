//! Spike: Evaluate reedline for ion TUI
//!
//! Run with: cargo run --example reedline_spike
//!
//! Tests:
//! 1. Multi-line input (via Validator)
//! 2. External printer for concurrent output
//! 3. History support

use reedline::{
    DefaultPrompt, DefaultPromptSegment, ExternalPrinter, Reedline, Signal, ValidationResult,
    Validator,
};
use std::thread;
use std::time::Duration;

/// Validator that allows multi-line input
/// Empty line or line ending with semicolon completes input
struct MultiLineValidator;

impl Validator for MultiLineValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        // Allow multi-line: incomplete if line ends with backslash or open bracket
        let trimmed = line.trim();
        if trimmed.ends_with('\\')
            || trimmed.ends_with('{')
            || trimmed.ends_with('(')
            || trimmed.ends_with('[')
        {
            ValidationResult::Incomplete
        } else {
            ValidationResult::Complete
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== reedline spike ===");
    println!("Commands: quit, async (start background messages)");
    println!("Multi-line: end line with \\ or {{ to continue");
    println!();

    // Create external printer for concurrent output
    let printer = ExternalPrinter::default();

    // Create reedline with external printer and validator
    let mut line_editor = Reedline::create()
        .with_external_printer(printer.clone())
        .with_validator(Box::new(MultiLineValidator));

    // Create a simple prompt
    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("> ".to_string()),
        DefaultPromptSegment::Empty,
    );

    // Spawn background task for async output test
    let printer_clone = printer.clone();
    let _bg_handle = thread::spawn(move || {
        thread::sleep(Duration::from_secs(5));
        for i in 1..=3 {
            thread::sleep(Duration::from_secs(2));
            let _ = printer_clone.print(format!(
                "[Background message {} - type while this appears!]",
                i
            ));
        }
    });

    loop {
        match line_editor.read_line(&prompt)? {
            Signal::Success(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                match line {
                    "quit" | "exit" => {
                        printer.print("Goodbye!".to_string())?;
                        break;
                    }
                    "async" => {
                        printer.print(
                            "[Starting async output test - type while messages appear]".to_string(),
                        )?;
                        let p = printer.clone();
                        thread::spawn(move || {
                            for i in 1..=5 {
                                thread::sleep(Duration::from_millis(800));
                                let _ = p.print(format!("[Async message {} of 5]", i));
                            }
                            let _ = p.print("[Async test complete]".to_string());
                        });
                    }
                    _ => {
                        // Show what was typed (including multi-line)
                        let line_count = line.lines().count();
                        if line_count > 1 {
                            printer.print(format!("You typed {} lines:", line_count))?;
                            for (i, l) in line.lines().enumerate() {
                                printer.print(format!("  {}: {}", i + 1, l))?;
                            }
                        } else {
                            printer.print(format!("You typed: {}", line))?;
                        }
                    }
                }
            }
            Signal::CtrlD => {
                printer.print("[EOF - Ctrl+D]".to_string())?;
                break;
            }
            Signal::CtrlC => {
                printer.print("[Interrupted - Ctrl+C]".to_string())?;
                // Continue instead of exit
            }
        }
    }

    println!("\n=== Spike complete ===");
    Ok(())
}
