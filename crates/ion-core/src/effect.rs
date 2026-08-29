//! Typed durable external-effect vocabulary.
//!
//! SQLite currently stores an effect kind plus a JSON payload for a compact,
//! inspectable v0 schema. The rest of Ion must not depend on that encoding.
//! This module is the one translation boundary between the storage encoding
//! and typed model/tool/maintenance plans used by runtime and recovery.

use serde_json::Value;

use crate::context::ContextPlan;
use crate::harness::HarnessProfile;
use crate::ids::{EffectId, OperationId};
use crate::provider::ModelConfig;
use crate::store::EffectRecord;
use crate::tool::{CanonicalTarget, RecoveryClass, ToolCall, ToolCallId};

/// Prompt-cache expectation captured with a model step for diagnostics and
/// evaluation. It is metadata, not a correctness or replay decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CacheExpectation {
    Unsupported,
    ColdStart,
    PrefixReuseExpected,
    PrefixChanged,
}

fn legacy_harness_profile() -> HarnessProfile {
    HarnessProfile::default_v1()
}

/// Exact immutable input for one ordinary model-step effect (DESIGN.md §5).
///
/// The initial v0 durable encoding predates the explicit harness-profile field,
/// so deserializing an omitted field means exactly `ion/default@1`. New typed
/// writers include it explicitly; this is a frozen compatibility rule rather
/// than whatever the launch default may mean in a future release.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ModelStepPlan {
    pub step: u64,
    pub model: ModelConfig,
    pub plan: ContextPlan,
    pub capability_snapshot_id: String,
    pub context_manifest_id: String,
    #[serde(default = "legacy_harness_profile")]
    pub harness_profile: HarnessProfile,
    pub prefix_fingerprint: String,
    pub cache_expectation: CacheExpectation,
}

/// Exact durable input for one harness-owned compaction model invocation.
/// Compaction remains a distinct maintenance purpose in the operation machine;
/// it does not become a second agent loop.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CompactionInvocation {
    pub step: u64,
    pub model: ModelConfig,
    pub plan: ContextPlan,
    #[serde(default = "legacy_harness_profile")]
    pub harness_profile: HarnessProfile,
}

/// Exact durable input for an admitted tool invocation.
///
/// `operation_id` intentionally is not encoded here: the durable effect row is
/// already owned by one operation. Recovery supplies that authoritative id
/// when reconstructing a [`ToolCall`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ToolInvocation {
    pub tool: String,
    pub arguments: Value,
    pub call_id: ToolCallId,
    pub canonical: Option<CanonicalTarget>,
    pub reconciliation: Option<Value>,
}

impl ToolInvocation {
    #[must_use]
    pub(crate) fn into_call(self, operation_id: OperationId) -> ToolCall {
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
pub(crate) enum DurableEffect {
    ModelStep(ModelStepPlan),
    Tool(ToolInvocation),
    Compaction(CompactionInvocation),
}

impl DurableEffect {
    fn kind(&self) -> String {
        match self {
            Self::ModelStep(_) => "model_step".to_owned(),
            Self::Tool(invocation) => format!("tool:{}", invocation.tool),
            Self::Compaction(_) => "compaction".to_owned(),
        }
    }

    fn encode(&self) -> Value {
        match self {
            Self::ModelStep(plan) => serde_json::to_value(plan),
            Self::Tool(invocation) => serde_json::to_value(invocation),
            Self::Compaction(invocation) => serde_json::to_value(invocation),
        }
        .expect("durable effect inputs are serializable")
    }
}

impl EffectRecord {
    /// Construct an effect record from typed semantic input. Runtime code should
    /// use this rather than assembling `kind`/JSON payloads by hand as call
    /// sites are migrated into their ownership modules.
    #[must_use]
    pub(crate) fn new(
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

    /// Decode the SQLite/checkpoint representation at one boundary. A mismatch
    /// is corruption or a programmer error; callers must fence or fail visibly
    /// instead of guessing.
    pub(crate) fn decode(&self) -> Result<DurableEffect, String> {
        match self.kind.as_str() {
            "model_step" => {
                let plan: ModelStepPlan = serde_json::from_value(self.effective_input.clone())
                    .map_err(|err| {
                        format!("invalid durable model-step effect {}: {err}", self.id)
                    })?;
                if !plan.harness_profile.is_supported() {
                    return Err(format!(
                        "unsupported durable model-step harness profile `{}` in effect {}",
                        plan.harness_profile.id, self.id
                    ));
                }
                Ok(DurableEffect::ModelStep(plan))
            }
            "compaction" => {
                let invocation: CompactionInvocation =
                    serde_json::from_value(self.effective_input.clone()).map_err(|err| {
                        format!("invalid durable compaction effect {}: {err}", self.id)
                    })?;
                if !invocation.harness_profile.is_supported() {
                    return Err(format!(
                        "unsupported durable compaction harness profile `{}` in effect {}",
                        invocation.harness_profile.id, self.id
                    ));
                }
                Ok(DurableEffect::Compaction(invocation))
            }
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
    pub(crate) fn next_attempt(&self) -> Self {
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

    fn model_step_plan() -> ModelStepPlan {
        ModelStepPlan {
            step: 7,
            model: ModelConfig {
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
            harness_profile: HarnessProfile::default_v1(),
            prefix_fingerprint: "prefix-1".to_owned(),
            cache_expectation: CacheExpectation::PrefixReuseExpected,
        }
    }

    #[test]
    fn model_step_effect_round_trips_exactly() {
        let plan = model_step_plan();
        let record = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::ModelStep(plan.clone()),
            RecoveryClass::ReplaySafe,
            1,
        );
        assert_eq!(record.kind, "model_step");
        assert_eq!(record.decode(), Ok(DurableEffect::ModelStep(plan)));
    }

    #[test]
    fn legacy_v0_model_step_decodes_to_the_frozen_default_profile() {
        let mut value = serde_json::to_value(model_step_plan()).expect("model step serializes");
        value
            .as_object_mut()
            .expect("model step is an object")
            .remove("harness_profile");
        let record = EffectRecord {
            id: EffectId::generate(),
            kind: "model_step".to_owned(),
            recovery_class: RecoveryClass::ReplaySafe,
            effective_input: value,
            attempt: 1,
        };
        let DurableEffect::ModelStep(decoded) = record.decode().expect("legacy step decodes")
        else {
            panic!("expected a model step");
        };
        assert_eq!(decoded.harness_profile, HarnessProfile::default_v1());
    }

    #[test]
    fn legacy_v0_compaction_decodes_to_the_frozen_default_profile() {
        let value = serde_json::json!({
            "step": 3,
            "model": model_step_plan().model,
            "plan": model_step_plan().plan,
        });
        let record = EffectRecord {
            id: EffectId::generate(),
            kind: "compaction".to_owned(),
            recovery_class: RecoveryClass::ReplaySafe,
            effective_input: value,
            attempt: 1,
        };
        let DurableEffect::Compaction(decoded) =
            record.decode().expect("legacy compaction decodes")
        else {
            panic!("expected compaction");
        };
        assert_eq!(decoded.harness_profile, HarnessProfile::default_v1());
    }

    #[test]
    fn unsupported_harness_profiles_fail_at_the_typed_decode_boundary() {
        let mut model_step = model_step_plan();
        model_step.harness_profile = HarnessProfile {
            id: "ion/default@2".to_owned(),
        };
        let model_record = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::ModelStep(model_step),
            RecoveryClass::ReplaySafe,
            1,
        );
        assert!(model_record.decode().is_err());

        let compaction_record = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::Compaction(CompactionInvocation {
                step: 3,
                model: model_step_plan().model,
                plan: model_step_plan().plan,
                harness_profile: HarnessProfile {
                    id: "ion/default@2".to_owned(),
                },
            }),
            RecoveryClass::ReplaySafe,
            1,
        );
        assert!(compaction_record.decode().is_err());
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
            DurableEffect::ModelStep(model_step_plan()),
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
