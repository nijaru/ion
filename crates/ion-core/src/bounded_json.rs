//! Capacity check before persisting externally supplied structured data.
//!
//! Counts JSON bytes without allocating a second encoded copy. The caller still owns
//! its domain-specific capacity and the handling of a completed external effect.

use std::io::{self, Write};

use serde::Serialize;

#[derive(Debug)]
pub(crate) enum CheckError {
    Capacity,
    Serialization(serde_json::Error),
}

pub(crate) fn check(value: &impl Serialize, limit: usize) -> Result<(), CheckError> {
    struct Counted {
        length: usize,
        limit: usize,
        exceeded: bool,
    }
    impl Write for Counted {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.length = self.length.saturating_add(bytes.len());
            if self.length > self.limit {
                self.exceeded = true;
                return Err(io::Error::other("JSON capacity exceeded"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Counted {
        length: 0,
        limit,
        exceeded: false,
    };
    match serde_json::to_writer(&mut writer, value) {
        Ok(()) => Ok(()),
        Err(_) if writer.exceeded => Err(CheckError::Capacity),
        Err(error) => Err(CheckError::Serialization(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_exact_boundary_and_stops_before_large_encoding() {
        let value = serde_json::json!({"text":"x".repeat(100_000)});
        let size = serde_json::to_vec(&value).unwrap().len();
        assert!(check(&value, 16).is_err_and(|error| matches!(error, CheckError::Capacity)));
        assert!(check(&value, size - 1).is_err_and(|error| matches!(error, CheckError::Capacity)));
        assert!(check(&value, size).is_ok());
    }
}
