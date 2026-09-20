//! Stable content digests used by durable semantic records.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut value = [0_u8; 32];
        value.copy_from_slice(&digest);
        Self(value)
    }

    pub fn of<T: Serialize>(value: &T) -> Result<Self, serde_json::Error> {
        serde_json::to_vec(value).map(|bytes| Self::of_bytes(&bytes))
    }

    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Debug for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ContentDigest({self})")
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_deterministic_and_sensitive_to_content() {
        let a = ContentDigest::of(&serde_json::json!({"a": 1, "b": 2})).expect("digest");
        let b = ContentDigest::of(&serde_json::json!({"a": 1, "b": 2})).expect("digest");
        let c = ContentDigest::of(&serde_json::json!({"a": 2, "b": 1})).expect("digest");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.to_string().len(), 64);
    }
}
