use std::io::{self, Write};

use super::TerminalSession;
use crate::{InputEvent, KeyCode, Modifiers};

// Credential entry is not a composer: no rendering, history or model admission.
const MAX_SECRET_BYTES: usize = 4096;

impl TerminalSession {
    /// Read a hidden single-line secret using the existing raw terminal lease.
    /// Quarantine type-ahead before the prompt and after success, rejection or
    /// cancellation. Dropping this future also quarantines its unread suffix.
    pub async fn read_secret(&mut self, prompt: &str) -> io::Result<String> {
        self.check_active()?;
        if prompt.chars().any(char::is_control) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "secret prompt contains control characters",
            ));
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
        result
    }
}

struct SecretEntry<'a> {
    terminal: &'a mut TerminalSession,
    finished: bool,
}

impl SecretEntry<'_> {
    async fn read(&mut self, prompt: &str) -> io::Result<String> {
        self.terminal.output()?.write_all(prompt.as_bytes())?;
        self.terminal.output()?.flush()?;
        let mut secret = String::new();
        loop {
            let event = self.terminal.next_input().await.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "secret input ended")
            })??;
            if consume(&mut secret, event)? {
                return Ok(secret);
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

fn consume(secret: &mut String, event: InputEvent) -> io::Result<bool> {
    match event {
        InputEvent::Key(key) if key.modifiers.contains(Modifiers::CONTROL) => match key.code {
            KeyCode::Char('c') => {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "secret entry cancelled",
                ));
            }
            KeyCode::Char('d') if secret.is_empty() => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "secret input ended",
                ));
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
                    return Err(invalid_secret());
                }
                secret.push(ch);
            }
            _ => {}
        },
        InputEvent::Paste(text) => {
            if text.chars().any(char::is_control) || secret.len() + text.len() > MAX_SECRET_BYTES {
                return Err(invalid_secret());
            }
            secret.push_str(&text);
        }
        InputEvent::Rejected(_) => return Err(invalid_secret()),
        _ => {}
    }
    Ok(false)
}

fn invalid_secret() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "secret must be a valid single line of at most 4096 bytes",
    )
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
        for event in [
            InputEvent::Paste("line\nother".into()),
            InputEvent::Paste("x".repeat(MAX_SECRET_BYTES)),
            InputEvent::Rejected(InputError::InvalidUtf8),
            InputEvent::Key(KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL)),
        ] {
            assert!(consume(&mut value, event).is_err());
            assert_eq!(value, "abCD");
        }
        assert!(
            consume(
                &mut value,
                InputEvent::Key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE))
            )
            .unwrap()
        );
    }
}
