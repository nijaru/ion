//! Immutable transcript and context-boundary entries.

use serde::{Deserialize, Serialize};

use crate::{
    AttemptId, BlobRef, ConversationId, EntryId, InputId, InvocationId, SemanticCompatibilityId,
    TranscriptMessage,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: EntryId,
    pub conversation: ConversationId,
    pub data: EntryData,
    /// Provider-neutral model-visible content. ContextBoundary entries usually
    /// have no direct projection and instead change how earlier history is rendered.
    pub projection: Vec<TranscriptMessage>,
}

impl Entry {
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match &self.data {
            EntryData::UserInput { .. } => "user",
            EntryData::Assistant { .. } => "assistant",
            EntryData::ToolResult { .. } => "tool_result",
            EntryData::ContextBoundary(_) => "context_boundary",
            EntryData::Notice { .. } => "notice",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EntryData {
    UserInput {
        input: InputId,
    },
    Assistant {
        step: crate::StepId,
    },
    ToolResult {
        invocation: InvocationId,
    },
    ContextBoundary(Box<ContextBoundary>),
    Notice {
        kind: String,
        detail: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextBoundary {
    pub source_cutoff: Option<EntryId>,
    /// Exact active-turn input evidence that must survive this boundary.
    pub retained_inputs: Vec<InputId>,
    pub checkpoint: ContinuationCheckpoint,
    pub raw_tail: EntryRange,
    pub checkpoint_schema: SemanticCompatibilityId,
    pub compactor: crate::ProviderBindingId,
    pub opaque_provider_artifact: Option<BlobRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRange {
    pub start: Option<EntryId>,
    pub end: Option<EntryId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuationCheckpoint {
    pub goals: Vec<String>,
    pub constraints: Vec<String>,
    pub done: Vec<String>,
    pub in_progress: Vec<String>,
    pub blocked: Vec<String>,
    pub decisions: Vec<CheckpointDecision>,
    pub evidence: Vec<EvidenceRef>,
    pub unresolved: Vec<String>,
    pub next_action: Option<String>,
    pub terminal_condition: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointDecision {
    pub decision: String,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceRef {
    Entry(EntryId),
    ModelAttempt(AttemptId),
    ToolAttempt(AttemptId),
    Invocation(InvocationId),
    Blob(crate::ContentDigest),
}
