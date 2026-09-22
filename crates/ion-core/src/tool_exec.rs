//! Logical tool calls and immutable physical execution attempts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AttemptId, BlobRef, ContentDigest, EgressRealm, EntryId, InvocationId, SemanticCompatibilityId,
    StepId, ToolBindingId,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedAction {
    pub binding: ToolBindingId,
    pub arguments: Value,
    pub digest: ContentDigest,
    pub egress: EgressRealm,
    pub workspace_revision: Option<u64>,
    pub base_facts: Vec<BaseFact>,
}

impl PreparedAction {
    pub fn new(
        binding: ToolBindingId,
        arguments: Value,
        egress: EgressRealm,
        workspace_revision: Option<u64>,
        base_facts: Vec<BaseFact>,
    ) -> Result<Self, serde_json::Error> {
        let digest = ContentDigest::of(&(
            &binding,
            &arguments,
            &egress,
            workspace_revision,
            &base_facts,
        ))?;
        Ok(Self {
            binding,
            arguments,
            digest,
            egress,
            workspace_revision,
            base_facts,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseFact {
    pub path: String,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub id: InvocationId,
    pub step: StepId,
    pub assistant_entry: EntryId,
    pub source_index: u32,
    pub origin_provider_call_id: Option<String>,
    pub binding: ToolBindingId,
    pub prepared: PreparedAction,
    pub approval: ApprovalState,
    pub exchange: ToolExchangeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalState {
    NotRequired,
    Pending,
    Approved { expires_at_unix_ms: Option<i64> },
    Denied { reason: String },
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub value: Value,
    pub is_error: bool,
    pub truncated: bool,
    pub full_output: Option<BlobRef>,
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
