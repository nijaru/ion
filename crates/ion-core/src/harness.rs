//! Versioned model-facing harness policy identity (DESIGN.md §14.9).
//!
//! Harness profiles may evolve projection/tool-presentation/compaction
//! heuristics, but never durable operation, policy, effect, or recovery
//! semantics. v0 starts with one explicit profile rather than hidden globals.

use sha2::{Digest, Sha256};

/// Stable identity for the initial direct-tool Ion harness.
pub const DEFAULT_HARNESS_PROFILE_ID: &str = "ion/default@1";

/// Canonical material whose digest identifies the behavior of the default
/// profile. Changing model-facing harness behavior requires a new id and/or
/// fingerprint; it must never silently reinterpret an in-flight model step.
const DEFAULT_HARNESS_PROFILE_MATERIAL: &str =
    "ion/default@1;context=baseline-v1;tools=direct-v1;compaction=baseline-v1;budget=baseline-v1";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HarnessProfile {
    pub id: String,
    pub fingerprint: String,
}

impl HarnessProfile {
    #[must_use]
    pub fn default_v1() -> Self {
        Self {
            id: DEFAULT_HARNESS_PROFILE_ID.to_owned(),
            fingerprint: hex_digest(DEFAULT_HARNESS_PROFILE_MATERIAL.as_bytes()),
        }
    }

    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self == &Self::default_v1()
    }
}

impl Default for HarnessProfile {
    fn default() -> Self {
        Self::default_v1()
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_identity_is_stable_and_self_consistent() {
        let profile = HarnessProfile::default_v1();
        assert_eq!(profile.id, DEFAULT_HARNESS_PROFILE_ID);
        assert_eq!(profile.fingerprint.len(), 64);
        assert!(profile.is_consistent());
        assert_eq!(profile, HarnessProfile::default());
    }
}
