//! Policy gate for concrete actions (DESIGN.md §17).
//!
//! Trust (§17.2) and approval (§17.1) are separate concerns; this module
//! is only the approval half: given a canonical tool invocation, decide
//! whether it may execute. The policy always sees the same effective
//! input the executor will use — canonicalization happens before the
//! policy decision (§17.3).

use std::{collections::HashSet, sync::Arc};

use crate::tool::CanonicalTarget;

/// Pi-extension parity: the protected-paths list the user's pi setup
/// ships (protected-paths.ts). Ion's policy layer owns the same
/// protection: write/edit to these paths denies wherever they appear
/// in the project tree.
pub const PI_PROTECTED_PATHS: &[&str] = &[
    ".env",
    ".env.",
    ".git/",
    "node_modules/",
    ".chezmoidata.yaml",
    ".chezmoidata.yaml.age",
    ".config/age/keys.txt",
];

/// One policy decision for one canonical invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Execute as admitted.
    Allow,
    /// Model-visible denial: the tool never starts and the model sees
    /// the reason so it can choose another path (§17.4).
    Deny(String),
    /// The action needs an approval. Non-interactive operation
    /// terminates the operation with `ApprovalRequired` instead of
    /// inviting a retry loop (§17.4).
    ApprovalRequired,
}

/// The approval policy for one runtime.
pub trait PolicyEngine: Send + Sync + 'static {
    fn decide(&self, tool: &str, target: &CanonicalTarget) -> PolicyDecision;
}

/// v0 default: local reads and file mutations run; `bash` requires an
/// explicit grant because its side effects are unbounded and its
/// recovery class is NeverReplay (§12.4).
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultPolicy;

impl PolicyEngine for DefaultPolicy {
    fn decide(&self, _tool: &str, target: &CanonicalTarget) -> PolicyDecision {
        match target {
            CanonicalTarget::Path { .. } => PolicyDecision::Allow,
            // Unbounded side effects: local shell and remote MCP/extension
            // effects both require an explicit grant (§12.4, §19.2).
            CanonicalTarget::Command { .. } | CanonicalTarget::Remote { .. } => {
                PolicyDecision::ApprovalRequired
            }
        }
    }
}

/// The documented non-interactive grant mechanism (§17.2, §17.4): the
/// caller supplies the exact tools that may execute; everything else
/// requires an approval no non-interactive caller can give.
#[derive(Debug, Clone)]
pub struct AllowlistPolicy {
    allowed: HashSet<String>,
}

/// Pi's protected-paths matcher, ported exactly: a leading slash is
/// stripped, then an entry matches when the normalized path equals
/// it, sits directly beneath it (`entry + "/"`), or contains it as a
/// whole component (`"/" + entry`). Entries ending in `/` are
/// directory prefixes and never match exact equality.
fn is_protected_path(path: &str, entries: &[String]) -> bool {
    let normalized = path.strip_prefix('/').unwrap_or(path);
    entries.iter().any(|entry| {
        if entry.ends_with('/') {
            normalized.starts_with(entry.as_str()) || normalized.contains(&format!("/{entry}"))
        } else {
            normalized == entry
                || normalized.starts_with(&format!("{entry}/"))
                || normalized.contains(&format!("/{entry}"))
        }
    })
}

/// A deny-list for file mutations layered over any engine: write and
/// edit to a protected path deny before the inner policy (and before
/// any grant — an allow-list cannot unprotect a protected path, just
/// as pi's extension blocks regardless of other permission state).
/// Reads, search, and everything else fall through untouched.
pub struct ProtectedPathsPolicy {
    inner: Arc<dyn PolicyEngine>,
    protected: Vec<String>,
}

impl ProtectedPathsPolicy {
    #[must_use]
    pub fn new(
        inner: Arc<dyn PolicyEngine>,
        protected: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            inner,
            protected: protected.into_iter().map(Into::into).collect(),
        }
    }
}

impl PolicyEngine for ProtectedPathsPolicy {
    fn decide(&self, tool: &str, target: &CanonicalTarget) -> PolicyDecision {
        if matches!(tool, "write" | "edit")
            && let CanonicalTarget::Path { path } = target
            && let Some(path) = path.to_str()
            && is_protected_path(path, &self.protected)
        {
            return PolicyDecision::Deny(format!("Path \"{path}\" is protected"));
        }
        self.inner.decide(tool, target)
    }
}

impl AllowlistPolicy {
    #[must_use]
    pub fn new(allowed: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            allowed: allowed.into_iter().map(Into::into).collect(),
        }
    }
}

impl PolicyEngine for AllowlistPolicy {
    fn decide(&self, tool: &str, _target: &CanonicalTarget) -> PolicyDecision {
        if self.allowed.contains(tool) {
            PolicyDecision::Allow
        } else {
            PolicyDecision::ApprovalRequired
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_paths_match_pi_extension_grammar() {
        let entries: Vec<String> = PI_PROTECTED_PATHS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        // Exact matches and direct children.
        assert!(is_protected_path(".env", &entries));
        assert!(is_protected_path(".git/config", &entries));
        assert!(is_protected_path("node_modules/pkg/index.js", &entries));
        // The matcher sees ion's absolute canonical paths.
        assert!(is_protected_path("/tmp/proj/.env", &entries));
        assert!(is_protected_path("/tmp/proj/.git/HEAD", &entries));
        assert!(is_protected_path("/tmp/proj/sub/node_modules/x", &entries));
        assert!(is_protected_path(
            "/tmp/proj/.config/age/keys.txt",
            &entries
        ));
        // The `.env.` entry matches exactly, as a directory prefix, or
        // with a slash before it — so root-level `.env.local` is NOT
        // protected (pi's extension behaves the same way), but
        // `sub/.env.local` is.
        assert!(!is_protected_path(".env.local", &entries));
        assert!(is_protected_path("sub/.env.local", &entries));
        assert!(is_protected_path(".env./nested", &entries));
        // Words merely containing "env" never deny.
        assert!(!is_protected_path("src/env.rs", &entries));
        assert!(!is_protected_path("renv.py", &entries));
        // `.chezmoidata.yaml` is exact-or-child, not substring.
        assert!(is_protected_path(".chezmoidata.yaml", &entries));
        assert!(!is_protected_path("my.chezmoidata.yaml", &entries));
        // Ordinary files never deny.
        assert!(!is_protected_path("src/main.rs", &entries));
    }

    #[test]
    fn protected_paths_deny_only_write_and_edit() {
        let policy = ProtectedPathsPolicy::new(
            std::sync::Arc::new(DefaultPolicy),
            PI_PROTECTED_PATHS.iter().map(|s| (*s).to_string()),
        );
        let deny_target = CanonicalTarget::Path {
            path: "/tmp/proj/.env".into(),
        };
        assert_eq!(
            policy.decide("write", &deny_target),
            PolicyDecision::Deny("Path \"/tmp/proj/.env\" is protected".to_owned())
        );
        assert_eq!(
            policy.decide("edit", &deny_target),
            PolicyDecision::Deny("Path \"/tmp/proj/.env\" is protected".to_owned())
        );
        // Reads pass through to the inner policy.
        assert_eq!(policy.decide("read", &deny_target), PolicyDecision::Allow);
        // Non-protected mutations stay allowed.
        let plain = CanonicalTarget::Path {
            path: "/tmp/proj/src/main.rs".into(),
        };
        assert_eq!(policy.decide("write", &plain), PolicyDecision::Allow);
    }

    #[test]
    fn protected_paths_layer_over_allowlist_grants() {
        // A --allow write grant cannot unprotect .env, exactly as
        // pi's extension blocks regardless of other permission state.
        let policy = ProtectedPathsPolicy::new(
            std::sync::Arc::new(AllowlistPolicy::new(["write"])),
            PI_PROTECTED_PATHS.iter().map(|s| (*s).to_string()),
        );
        let target = CanonicalTarget::Path {
            path: "/tmp/proj/.git/config".into(),
        };
        assert_eq!(
            policy.decide("write", &target),
            PolicyDecision::Deny("Path \"/tmp/proj/.git/config\" is protected".to_owned())
        );
    }
}
