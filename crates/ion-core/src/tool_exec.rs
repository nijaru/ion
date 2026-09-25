//! Logical tool calls and immutable physical execution attempts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AttemptId, BlobRef, ContentDigest, EgressRealm, EntryId, InvocationId, SemanticCompatibilityId,
    StepId, ToolBindingId, WorkspaceBinding,
};

/// Required authority, not proof of confinement. The trusted executor must
/// enforce the declared class and recheck live policy at physical admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolAuthority {
    ReadOnly,
    WorkspaceMutation,
    /// Unconfined execution necessarily also requires mutation authority.
    UnconfinedExecution,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedAction {
    pub binding: ToolBindingId,
    pub arguments: Value,
    pub digest: ContentDigest,
    pub egress: EgressRealm,
    pub authority: ToolAuthority,
    pub workspace_revision: Option<u64>,
    pub base_facts: Vec<BaseFact>,
}

impl PreparedAction {
    pub fn new(
        binding: ToolBindingId,
        arguments: Value,
        egress: EgressRealm,
        authority: ToolAuthority,
        workspace_revision: Option<u64>,
        base_facts: Vec<BaseFact>,
    ) -> Result<Self, serde_json::Error> {
        let digest = ContentDigest::of(&(
            &binding,
            &arguments,
            &egress,
            authority,
            workspace_revision,
            &base_facts,
        ))?;
        Ok(Self {
            binding,
            arguments,
            digest,
            egress,
            authority,
            workspace_revision,
            base_facts,
        })
    }
    /// Approval can satisfy live policy, but cannot widen this frozen ceiling.
    #[must_use]
    pub fn permitted_by(&self, ceiling: &crate::AuthorityCeiling) -> bool {
        ceiling.permits(&self.egress)
            && (matches!(self.egress, EgressRealm::Local) || ceiling.remote_tools)
            && match self.authority {
                ToolAuthority::ReadOnly => true,
                ToolAuthority::WorkspaceMutation => ceiling.workspace_mutation,
                ToolAuthority::UnconfinedExecution => {
                    ceiling.workspace_mutation && ceiling.unconfined_execution
                }
            }
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;

    #[test]
    fn requirements_are_digest_bound_and_never_defaulted() {
        let action = |authority| {
            PreparedAction::new(
                ToolBindingId::new("tool").unwrap(),
                serde_json::json!({}),
                EgressRealm::Local,
                authority,
                None,
                vec![],
            )
            .unwrap()
        };
        let read = action(ToolAuthority::ReadOnly);
        let mutation = action(ToolAuthority::WorkspaceMutation);
        let exec = action(ToolAuthority::UnconfinedExecution);
        assert_ne!(read.digest, mutation.digest);
        assert_ne!(mutation.digest, exec.digest);
        assert_ne!(read.digest, exec.digest);
        let mut encoded = serde_json::to_value(read).unwrap();
        encoded.as_object_mut().unwrap().remove("authority");
        assert!(serde_json::from_value::<PreparedAction>(encoded).is_err());
    }

    #[test]
    fn every_required_permission_and_realm_must_be_inside_ceiling() {
        for mutation in [false, true] {
            for unconfined in [false, true] {
                for remote in [false, true] {
                    for allowed_realm in [false, true] {
                        for realm in [EgressRealm::Local, EgressRealm::Remote("service".into())] {
                            let ceiling = crate::AuthorityCeiling {
                                workspace_mutation: mutation,
                                unconfined_execution: unconfined,
                                remote_tools: remote,
                                egress_realms: if allowed_realm {
                                    vec![realm.clone()]
                                } else {
                                    vec![]
                                },
                            };
                            for authority in [
                                ToolAuthority::ReadOnly,
                                ToolAuthority::WorkspaceMutation,
                                ToolAuthority::UnconfinedExecution,
                            ] {
                                let action = PreparedAction::new(
                                    ToolBindingId::new("tool").unwrap(),
                                    serde_json::json!({}),
                                    realm.clone(),
                                    authority,
                                    None,
                                    vec![],
                                )
                                .unwrap();
                                let required = match authority {
                                    ToolAuthority::ReadOnly => true,
                                    ToolAuthority::WorkspaceMutation => mutation,
                                    ToolAuthority::UnconfinedExecution => mutation && unconfined,
                                };
                                assert_eq!(
                                    action.permitted_by(&ceiling),
                                    required
                                        && allowed_realm
                                        && (realm == EgressRealm::Local || remote)
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseFact {
    pub path: String,
    pub digest: ContentDigest,
}

/// Immutable admission disposition for one provider tool call. An unavailable
/// preparer cannot supply a PreparedAction, and no attempt may be derived from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToolPreparation {
    Ready(PreparedAction),
    Unavailable,
}

impl ToolPreparation {
    #[must_use]
    pub fn ready(&self) -> Option<&PreparedAction> {
        match self {
            Self::Ready(action) => Some(action),
            Self::Unavailable => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub id: InvocationId,
    pub step: StepId,
    pub assistant_entry: EntryId,
    pub source_index: u32,
    pub origin_provider_call_id: Option<String>,
    pub binding: ToolBindingId,
    pub preparation: ToolPreparation,
    pub approval: ApprovalState,
    pub exchange: ToolExchangeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalState {
    NotRequired,
    Pending,
    Approved {
        action_digest: ContentDigest,
        implementation: SemanticCompatibilityId,
        executor: SemanticCompatibilityId,
        workspace: WorkspaceBinding,
        expires_at_unix_ms: i64,
    },
    Denied {
        reason: String,
    },
}

impl ApprovalState {
    #[must_use]
    pub fn permits(
        &self,
        action: &PreparedAction,
        implementation: &SemanticCompatibilityId,
        executor: &SemanticCompatibilityId,
        workspace: &WorkspaceBinding,
        now_unix_ms: i64,
    ) -> bool {
        matches!(self, Self::Approved {
            action_digest,
            implementation: approved_implementation,
            executor: approved_executor,
            workspace: approved_workspace,
            expires_at_unix_ms,
        } if action_digest == &action.digest
            && approved_implementation == implementation
            && approved_executor == executor
            && approved_workspace == workspace
            && now_unix_ms < *expires_at_unix_ms)
    }
}

/// Authenticated host/user control, never model or ordinary message content.
/// An approval can satisfy an `ask` only for the exact frozen action and executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approve { expires_at_unix_ms: i64 },
    Deny { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToolExchangeState {
    Pending,
    OutcomeReady {
        source: OutcomeSource,
        result: ToolResult,
    },
    Materialized {
        entry: EntryId,
        source: OutcomeSource,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeSource {
    Attempt(AttemptId),
    CancelledBeforeStart,
    DeniedApproval,
    Unavailable,
    AcceptedUnknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolAttempt {
    pub id: AttemptId,
    pub invocation: InvocationId,
    pub ordinal: u32,
    pub generation: u64,
    pub executor: SemanticCompatibilityId,
    pub progress: Option<ProgressCheckpoint>,
    pub state: ToolAttemptState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToolAttemptState {
    IntentCommitted {
        start_receipt: Option<StartReceipt>,
    },
    NotStarted {
        reason: String,
    },
    Settled {
        result: ToolResult,
        effect: EffectSummary,
        receipt: Option<StartReceipt>,
        /// Explicit backend classification, independent of model-visible failure.
        retryable: bool,
    },
    Indeterminate {
        reason: String,
        receipt: Option<StartReceipt>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartReceipt {
    pub kind: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressCheckpoint {
    pub sequence: u64,
    pub preview: String,
    pub dropped_bytes: u64,
}

/// A bounded model-visible value is never silently presented as complete output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputCapture {
    CompleteInline,
    CompleteArtifact {
        full_output: BlobRef,
    },
    Incomplete {
        reason: OutputLoss,
        retained_bytes: u64,
        /// None means the total observed output size is unknown.
        observed_bytes: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputLoss {
    Quota,
    Stopped,
    BackendCapacity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Complete inline content or a bounded preview, as specified by capture.
    pub value: Value,
    /// Model-visible tool failure, independent of execution/effect certainty.
    pub is_error: bool,
    pub capture: OutputCapture,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EffectSummary {
    NoMutation,
    KnownChanges { paths: Vec<String> },
    MayHaveMutated,
    Receipt { kind: String, data: Value },
}

impl EffectSummary {
    #[must_use]
    pub const fn permits_automatic_repeat(&self) -> bool {
        matches!(self, Self::NoMutation)
    }
}
