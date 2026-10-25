use std::io::{self, Write};

use super::TerminalSession;
use crate::{InputEvent, KeyCode, Modifiers};

// Credential entry is not a composer: no rendering, history or model admission.
const MAX_SECRET_BYTES: usize = 4096;

/// User entry outcomes, distinct from failure of the physical terminal lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretRejection {
    Cancelled,
    Ended,
    Invalid,
    InvalidPrompt,
}

impl std::fmt::Display for SecretRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Cancelled => "secret entry cancelled",
            Self::Ended => "secret input ended",
            Self::Invalid => "secret must be a valid single line of at most 4096 bytes",
            Self::InvalidPrompt => "secret prompt contains control characters",
        })
    }
}
impl std::error::Error for SecretRejection {}

impl TerminalSession {
    /// Read a hidden single-line secret using the existing raw terminal lease.
    /// Quarantine type-ahead before the prompt and after success, rejection or
    /// cancellation. Dropping this future also quarantines its unread suffix.
    /// Only the inner rejection permits returning to ordinary input admission.
    pub async fn read_secret(
        &mut self,
        prompt: &str,
    ) -> io::Result<Result<String, SecretRejection>> {
        self.check_active()?;
        if prompt.chars().any(char::is_control) {
            return Ok(Err(SecretRejection::InvalidPrompt));
        }
        self.input
            .quarantine()
            .inspect_err(|_| self.lock().fail())?;
        self.leave_alt_screen()?;
        let mut entry = SecretEntry {
            terminal: self,
            finished: false,
        };
        let result = entry.read(prompt).await;
        if entry.terminal.input.secret_paste_is_open() {
            entry
                .terminal
                .output()?
                .write_all(b"\r\nSecret paste discarded; waiting for its closing marker.\r\n")?;
            entry.terminal.output()?.flush()?;
            entry.terminal.input.settle_secret_paste().await?;
        }
        entry.finish()?;
        // Quarantine may restart intake successfully after a one-shot read fault.
        // It cannot restore a lease whose physical input has already failed.
        if result.is_err() {
            self.lock().fail();
        }
        result
    }
}

struct SecretEntry<'a> {
    terminal: &'a mut TerminalSession,
    finished: bool,
}

impl SecretEntry<'_> {
    async fn read(&mut self, prompt: &str) -> io::Result<Result<String, SecretRejection>> {
        self.terminal.output()?.write_all(prompt.as_bytes())?;
        self.terminal.output()?.flush()?;
        let mut secret = String::new();
        loop {
            let event = self.terminal.next_input().await.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "secret input ended")
            })??;
            match consume(&mut secret, event) {
                Ok(true) => return Ok(Ok(secret)),
                Ok(false) => {}
                Err(rejection) => return Ok(Err(rejection)),
            }
        }
    }

    fn finish(mut self) -> io::Result<()> {
        self.finished = true;
        let result = self.terminal.input.quarantine();
        if result.is_err() {
            self.terminal.lock().fail();
        }
        result?;
        self.terminal.output()?.write_all(b"\r\n")?;
        self.terminal.output()?.flush()
    }
}

impl Drop for SecretEntry<'_> {
    fn drop(&mut self) {
        if !self.finished && self.terminal.input.quarantine().is_err() {
            // Never return unquarantined secret bytes to ordinary intake.
            self.terminal.lock().fail();
        }
    }
}

fn consume(secret: &mut String, event: InputEvent) -> Result<bool, SecretRejection> {
    match event {
        InputEvent::Key(key) if key.modifiers.contains(Modifiers::CONTROL) => match key.code {
            KeyCode::Char('c') => {
                return Err(SecretRejection::Cancelled);
            }
            KeyCode::Char('d') if secret.is_empty() => {
                return Err(SecretRejection::Ended);
            }
            KeyCode::Char('u') => secret.clear(),
            KeyCode::Char('w') => {
                let end = secret
                    .trim_end()
                    .trim_end_matches(|ch: char| !ch.is_whitespace())
                    .len();
                secret.truncate(end);
            }
            _ => {}
        },
        InputEvent::Key(key) if !key.modifiers.contains(Modifiers::ALT) => match key.code {
            KeyCode::Enter => return Ok(true),
            KeyCode::Backspace => {
                secret.pop();
            }
            KeyCode::Char(ch) if !ch.is_control() => {
                if secret.len() + ch.len_utf8() > MAX_SECRET_BYTES {
                    return Err(SecretRejection::Invalid);
                }
                secret.push(ch);
            }
            _ => {}
        },
        InputEvent::Paste(text) => {
            if text.chars().any(char::is_control) || secret.len() + text.len() > MAX_SECRET_BYTES {
                return Err(SecretRejection::Invalid);
            }
            secret.push_str(&text);
        }
        InputEvent::Rejected(_) => return Err(SecretRejection::Invalid),
        _ => {}
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InputError, KeyEvent};

    #[test]
    fn secret_input_edits_without_accepting_rejected_or_partial_values() {
        let mut value = "ab🦀".to_owned();
        consume(
            &mut value,
            InputEvent::Key(KeyEvent::new(KeyCode::Backspace, Modifiers::NONE)),
        )
        .unwrap();
        assert_eq!(value, "ab");
        consume(&mut value, InputEvent::Paste("CD".into())).unwrap();
        assert_eq!(value, "abCD");
        for (event, rejection) in [
            (
                InputEvent::Paste("line\nother".into()),
                SecretRejection::Invalid,
            ),
            (
                InputEvent::Paste("x".repeat(MAX_SECRET_BYTES)),
                SecretRejection::Invalid,
            ),
            (
                InputEvent::Rejected(InputError::InvalidUtf8),
                SecretRejection::Invalid,
            ),
            (
                InputEvent::Key(KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL)),
                SecretRejection::Cancelled,
            ),
        ] {
            assert_eq!(consume(&mut value, event), Err(rejection));
            assert_eq!(value, "abCD");
        }
        assert_eq!(
            consume(
                &mut String::new(),
                InputEvent::Key(KeyEvent::new(KeyCode::Char('d'), Modifiers::CONTROL))
            ),
            Err(SecretRejection::Ended)
        );
        assert!(
            consume(
                &mut value,
                InputEvent::Key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
            )
            .unwrap()
        );
    }
}
