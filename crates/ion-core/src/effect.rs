//! Typed durable external-effect vocabulary.
//!
//! SQLite currently stores an effect kind plus a JSON payload for a compact,
//! inspectable v0 schema. The rest of Ion must not depend on that encoding.
//! This module is the one translation boundary between the storage encoding
//! and typed model/tool/maintenance invocations used by runtime and recovery.

use serde_json::Value;

use crate::context::ContextPlan;
use crate::ids::{EffectId, OperationId};
use crate::provider::ResolvedModel;
use crate::store::EffectRecord;
use crate::tool::{CanonicalTarget, RecoveryClass, ToolCall, ToolCallId};

/// Prompt-cache expectation captured with a model invocation for diagnostics
/// and evaluation. It is metadata, not a correctness or replay decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheExpectation {
    Unsupported,
    ColdStart,
    PrefixReuseExpected,
    PrefixChanged,
}

impl CacheExpectation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::ColdStart => "cold_start",
            Self::PrefixReuseExpected => "prefix_reuse_expected",
            Self::PrefixChanged => "prefix_changed",
        }
    }
}

/// Exact durable input for one ordinary model invocation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelInvocation {
    pub step: u64,
    pub model: ResolvedModel,
    pub plan: ContextPlan,
    pub capability_snapshot_id: String,
    pub context_manifest_id: String,
    pub prefix_fingerprint: String,
    pub cache_expectation: CacheExpectation,
}

/// Exact durable input for one harness-owned compaction model invocation.
/// Compaction remains a distinct purpose for now; later operation-state
/// normalization can represent both with a common model-invocation state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompactionInvocation {
    pub step: u64,
    pub model: ResolvedModel,
    pub plan: ContextPlan,
}

/// Exact durable input for an admitted tool invocation.
///
/// `operation_id` intentionally is not encoded here: the durable effect row is
/// already owned by one operation. Recovery supplies that authoritative id
/// when reconstructing a [`ToolCall`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolInvocation {
    pub tool: String,
    pub arguments: Value,
    pub call_id: ToolCallId,
    pub canonical: Option<CanonicalTarget>,
    pub reconciliation: Option<Value>,
}

impl ToolInvocation {
    #[must_use]
    pub fn into_call(self, operation_id: OperationId) -> ToolCall {
        ToolCall {
            operation_id,
            call_id: self.call_id,
            name: self.tool,
            arguments: self.arguments,
        }
    }
}

/// Typed meaning of an external effect. Storage encoding is deliberately not
/// exposed as an architectural contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableEffect {
    Model(ModelInvocation),
    Tool(ToolInvocation),
    Compaction(CompactionInvocation),
}

impl DurableEffect {
    fn kind(&self) -> String {
        match self {
            Self::Model(_) => "model_step".to_owned(),
            Self::Tool(invocation) => format!("tool:{}", invocation.tool),
            Self::Compaction(_) => "compaction".to_owned(),
        }
    }

    fn encode(&self) -> Value {
        match self {
            Self::Model(invocation) => serde_json::to_value(invocation),
            Self::Tool(invocation) => serde_json::to_value(invocation),
            Self::Compaction(invocation) => serde_json::to_value(invocation),
        }
        .expect("durable effect inputs are serializable")
    }
}

impl EffectRecord {
    /// Construct an effect record from typed semantic input. Runtime code should
    /// use this rather than assembling `kind`/JSON payloads by hand.
    #[must_use]
    pub fn new(
        id: EffectId,
        intent: DurableEffect,
        recovery_class: RecoveryClass,
        attempt: u64,
    ) -> Self {
        Self {
            id,
            kind: intent.kind(),
            recovery_class,
            effective_input: intent.encode(),
            attempt,
        }
    }

    /// Decode the legacy SQLite/checkpoint representation at one boundary.
    /// A mismatch is corruption or a programmer error; callers must fence or
    /// fail visibly instead of guessing.
    pub fn decode(&self) -> Result<DurableEffect, String> {
        match self.kind.as_str() {
            "model_step" => serde_json::from_value(self.effective_input.clone())
                .map(DurableEffect::Model)
                .map_err(|err| format!("invalid durable model effect {}: {err}", self.id)),
            "compaction" => serde_json::from_value(self.effective_input.clone())
                .map(DurableEffect::Compaction)
                .map_err(|err| format!("invalid durable compaction effect {}: {err}", self.id)),
            kind if kind.starts_with("tool:") => {
                let invocation: ToolInvocation =
                    serde_json::from_value(self.effective_input.clone())
                        .map_err(|err| format!("invalid durable tool effect {}: {err}", self.id))?;
                let encoded_name = kind.trim_start_matches("tool:");
                if invocation.tool != encoded_name {
                    return Err(format!(
                        "durable tool effect kind `{encoded_name}` disagrees with payload `{}`",
                        invocation.tool
                    ));
                }
                Ok(DurableEffect::Tool(invocation))
            }
            other => Err(format!("unknown durable effect kind `{other}`")),
        }
    }

    /// Create the next replay attempt without changing the exact semantic
    /// input. Recovery policy decides whether replay is allowed before calling
    /// this helper.
    #[must_use]
    pub fn next_attempt(&self) -> Self {
        Self {
            id: EffectId::generate(),
            kind: self.kind.clone(),
            recovery_class: self.recovery_class,
            effective_input: self.effective_input.clone(),
            attempt: self.attempt.saturating_add(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ModelCapabilities;

    fn model_invocation() -> ModelInvocation {
        ModelInvocation {
            step: 7,
            model: ResolvedModel {
                model_ref: "test/model".to_owned(),
                context_window: Some(131_072),
                capabilities: ModelCapabilities {
                    reasoning: true,
                    tool_calls: true,
                    prompt_cache: true,
                    streaming: true,
                },
            },
            plan: ContextPlan {
                system: "stable system".to_owned(),
                messages: Vec::new(),
            },
            capability_snapshot_id: "cap-1".to_owned(),
            context_manifest_id: "ctx-1".to_owned(),
            prefix_fingerprint: "prefix-1".to_owned(),
            cache_expectation: CacheExpectation::PrefixReuseExpected,
        }
    }

    #[test]
    fn model_effect_round_trips_exactly() {
        let invocation = model_invocation();
        let record = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::Model(invocation.clone()),
            RecoveryClass::ReplaySafe,
            1,
        );
        assert_eq!(record.kind, "model_step");
        assert_eq!(record.decode(), Ok(DurableEffect::Model(invocation)));
    }

    #[test]
    fn tool_kind_must_match_payload() {
        let invocation = ToolInvocation {
            tool: "read".to_owned(),
            arguments: serde_json::json!({"path": "README.md"}),
            call_id: 4,
            canonical: None,
            reconciliation: None,
        };
        let mut record = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::Tool(invocation),
            RecoveryClass::ReplaySafe,
            1,
        );
        record.kind = "tool:write".to_owned();
        assert!(record.decode().is_err());
    }

    #[test]
    fn recovery_supplies_operation_identity() {
        let invocation = ToolInvocation {
            tool: "read".to_owned(),
            arguments: serde_json::json!({"path": "README.md"}),
            call_id: 9,
            canonical: Some(CanonicalTarget::Path {
                path: std::path::PathBuf::from("/tmp/project/README.md"),
            }),
            reconciliation: None,
        };
        let operation_id = OperationId::generate();
        let call = invocation.into_call(operation_id);
        assert_eq!(call.operation_id, operation_id);
        assert_eq!(call.call_id, 9);
        assert_eq!(call.name, "read");
    }

    #[test]
    fn replay_attempt_preserves_input_and_changes_only_attempt_identity() {
        let record = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::Model(model_invocation()),
            RecoveryClass::ReplaySafe,
            3,
        );
        let replay = record.next_attempt();
        assert_ne!(replay.id, record.id);
        assert_eq!(replay.attempt, 4);
        assert_eq!(replay.kind, record.kind);
        assert_eq!(replay.recovery_class, record.recovery_class);
        assert_eq!(replay.effective_input, record.effective_input);
    }
}
