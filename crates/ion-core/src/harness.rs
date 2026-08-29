//! Versioned model-facing harness policy identity (DESIGN.md §14.9).
//!
//! Harness profiles may evolve projection/tool-presentation/compaction
//! heuristics, but never durable operation, policy, effect, or recovery
//! semantics. v0 starts with one explicit immutable profile id rather than
//! hidden globals. Exact model-step inputs remain the replay authority.

/// Stable immutable identity for the initial direct-tool Ion harness.
///
/// Changing model-facing harness policy requires a new profile id. A content
/// digest belongs here only once Ion has structured profile configuration whose
/// canonical bytes can actually be hashed; a synthetic digest would add false
/// precision without improving replay.
pub(crate) const DEFAULT_HARNESS_PROFILE_ID: &str = "ion/default@1";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct HarnessProfile {
    pub(crate) id: String,
}

impl HarnessProfile {
    #[must_use]
    pub(crate) fn default_v1() -> Self {
        Self {
            id: DEFAULT_HARNESS_PROFILE_ID.to_owned(),
        }
    }

    /// v0 only knows the frozen default profile. Unknown persisted profile ids
    /// must fail visibly rather than being interpreted with current defaults.
    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.id == DEFAULT_HARNESS_PROFILE_ID
    }
}

impl Default for HarnessProfile {
    fn default() -> Self {
        Self::default_v1()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_identity_is_stable_and_supported() {
        let profile = HarnessProfile::default_v1();
        assert_eq!(profile.id, DEFAULT_HARNESS_PROFILE_ID);
        assert!(profile.is_supported());
        assert_eq!(profile, HarnessProfile::default());
    }

    #[test]
    fn unknown_profile_is_not_silently_reinterpreted() {
        let profile = HarnessProfile {
            id: "ion/default@2".to_owned(),
        };
        assert!(!profile.is_supported());
    }
}
