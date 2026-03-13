/// The library's error type.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("layout error: {0}")]
    Layout(String),

    #[error("terminal size is too small: {width}x{height} (minimum 10x4)")]
    TerminalTooSmall { width: u16, height: u16 },
}

pub type Result<T> = std::result::Result<T, TuiError>;
