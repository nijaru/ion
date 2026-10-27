use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    PasteTooLarge,
    InvalidPasteUtf8,
    IncompletePaste,
    InvalidUtf8,
    IncompleteTerminalReply,
    TerminalReplyTooLarge,
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PasteTooLarge => write!(
                f,
                "Paste exceeds terminal input limit ({} bytes); paste discarded",
                super::paste::MAX_PASTE_BYTES
            ),
            Self::InvalidPasteUtf8 => f.write_str("Paste contains invalid UTF-8; paste discarded"),
            Self::IncompletePaste => {
                f.write_str("Paste ended without its closing marker; paste discarded")
            }
            Self::InvalidUtf8 => {
                f.write_str("Terminal input contains invalid UTF-8; invalid bytes discarded")
            }
            Self::IncompleteTerminalReply => {
                f.write_str("Terminal response timed out or ended incomplete; response discarded")
            }
            Self::TerminalReplyTooLarge => {
                f.write_str("Terminal response exceeds the input limit; response discarded")
            }
        }
    }
}
