//! OSC 7501 root-record publication. The terminal client owns state transitions;
//! this module owns the bounded wire encoding and physical write contract.

/// Root-record program state. Only interactions Ion actually exposes are
/// represented; ordinary idle is not blocked and no approval/question state is
/// invented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramStatus {
    Idle,
    Working,
    Done,
    Error,
    /// A real credential prompt is waiting for input. This is an actual auth
    /// block, not an ordinary idle composer or invented approval state.
    BlockedAuth,
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
            Self::BlockedAuth => b"\x1b]7501;state=blocked:kind=auth:app=ion\x1b\\",
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
            ProgramStatus::BlockedAuth,
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
