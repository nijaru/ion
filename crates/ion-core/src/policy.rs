//! Policy gate for concrete actions (DESIGN.md §17).
//!
//! Trust (§17.2) and approval (§17.1) are separate concerns; this module
//! is only the approval half: given a canonical tool invocation, decide
//! whether it may execute. The policy always sees the same effective
//! input the executor will use — canonicalization happens before the
//! policy decision (§17.3).

use std::collections::HashSet;

use crate::tool::CanonicalTarget;

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
    fn decide(&self, tool: &str, _target: &CanonicalTarget) -> PolicyDecision {
        match tool {
            "bash" => PolicyDecision::ApprovalRequired,
            "read" | "write" | "edit" | "search" | "find" => PolicyDecision::Allow,
            other => PolicyDecision::Deny(format!("unknown tool: {other}")),
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
