//! OSC 7501 root-record publication. The terminal client owns state transitions;
//! this module owns the bounded wire encoding and physical write contract.

/// Root-record program state. `blocked` is intentionally absent until Ion has a
/// real user-blocking interaction to report; ordinary idle is not blocked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramStatus {
    Idle,
    Working,
    Done,
    Error,
}

impl ProgramStatus {
    /// One complete root-record report. Every report repeats `app` because a
    /// report replaces the record completely.
    #[must_use]
    pub fn sequence(self) -> &'static [u8] {
        match self {
            Self::Idle => b"\x1b]7501;state=idle:app=ion\x1b\\",
            Self::Working => b"\x1b]7501;state=working:app=ion\x1b\\",
            Self::Done => b"\x1b]7501;state=done:app=ion\x1b\\",
            Self::Error => b"\x1b]7501;state=error:app=ion\x1b\\",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_complete_bounded_root_records() {
        for status in [
            ProgramStatus::Idle,
            ProgramStatus::Working,
            ProgramStatus::Done,
            ProgramStatus::Error,
        ] {
            let sequence = status.sequence();
            assert!(sequence.starts_with(b"\x1b]7501;"));
            assert!(sequence.ends_with(b"\x1b\\"));
            assert!(sequence.len() < 4096);
            assert!(sequence.windows(5).any(|part| part == b"app=i"));
            let body = std::str::from_utf8(&sequence[7..sequence.len() - 2]).unwrap();
            assert!(body.contains("state="), "{body}");
            assert!(body.contains("app=ion"), "{body}");
        }
    }
}
