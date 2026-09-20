//! Logical model requests and immutable physical dispatch evidence.

use ion_ai::{ModelResponse, ProviderErrorKind, Usage};
use serde::{Deserialize, Serialize};

use crate::{
    AttemptId, ContentDigest, EntryId, InputId, SemanticCompatibilityId, StepId, TurnId,
    TurnSettings,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelStep {
    pub id: StepId,
    pub turn: TurnId,
    pub ordinal: u32,
    pub purpose: StepPurpose,
    pub manifest: RequestManifest,
    pub disposition: StepDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepPurpose {
    Generate,
    Compact,
    Fallback { predecessor: StepId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepDisposition {
    Open,
    Selected(AttemptId),
    Superseded {
        reason: String,
        successor: Option<StepId>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestManifest {
    pub environment_digest: ContentDigest,
    pub settings: TurnSettings,
    pub context_boundary: Option<EntryId>,
    pub cutoff: Option<EntryId>,
    pub included_inputs: Vec<InputId>,
    pub assembly: SemanticCompatibilityId,
    pub semantic_digest: ContentDigest,
    pub provider_fingerprint: ProviderFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFingerprint {
    pub encoding: SemanticCompatibilityId,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelAttempt {
    pub id: AttemptId,
    pub step: StepId,
    pub ordinal: u32,
    pub generation: u64,
    pub timing: ModelAttemptTiming,
    pub cost_quote: Option<CostQuote>,
    pub state: ModelAttemptState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAttemptTiming {
    pub connect_timeout_ms: u64,
    pub response_timeout_ms: u64,
    pub deferred_poll_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostQuote {
    pub revision: String,
    pub reserved_microusd: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ModelAttemptState {
    IntentCommitted {
        start_receipt: Option<ProviderStartReceipt>,
    },
    NotStarted {
        reason: String,
    },
    Failed {
        failure: ProviderFailureEvidence,
        start_receipt: Option<ProviderStartReceipt>,
    },
    Indeterminate {
        reason: String,
        usage: Usage,
        start_receipt: Option<ProviderStartReceipt>,
    },
    ResponseReady {
        response: ModelResponse,
        start_receipt: Option<ProviderStartReceipt>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderFailureEvidence {
    pub kind: ProviderErrorKind,
    pub message: String,
    pub usage: Usage,
    pub provider_reported_cost_microusd: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderStartReceipt {
    pub kind: String,
    pub data: serde_json::Value,
}
