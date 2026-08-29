from pathlib import Path
import re


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one exact match, found {count}")
    file.write_text(text.replace(old, new, 1))


def sub_once(path: str, pattern: str, replacement: str) -> None:
    file = Path(path)
    text = file.read_text()
    new, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
    if count != 1:
        raise SystemExit(f"{path}: expected one regex match, found {count}")
    file.write_text(new)


# EffectRecord::decode is the one durable decoding boundary, including
# versioned harness-profile validation.
effect = "crates/ion-core/src/effect.rs"
replace_once(
    effect,
    '''            "model_step" => serde_json::from_value(self.effective_input.clone())
                .map(DurableEffect::ModelStep)
                .map_err(|err| format!("invalid durable model-step effect {}: {err}", self.id)),
            "compaction" => serde_json::from_value(self.effective_input.clone())
                .map(DurableEffect::Compaction)
                .map_err(|err| format!("invalid durable compaction effect {}: {err}", self.id)),''',
    '''            "model_step" => {
                let plan: ModelStepPlan = serde_json::from_value(self.effective_input.clone())
                    .map_err(|err| format!("invalid durable model-step effect {}: {err}", self.id))?;
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
            }''',
)
replace_once(
    effect,
    '''    #[test]
    fn tool_kind_must_match_payload() {''',
    '''    #[test]
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
    fn tool_kind_must_match_payload() {''',
)

# Persistence owns commit construction only; raw effect JSON interpretation is
# no longer duplicated here.
persistence = "crates/ion-core/src/runtime/persistence.rs"
sub_once(
    persistence,
    r'''\Ause crate::effect::\{CompactionInvocation, ModelStepPlan, ToolInvocation\};\nuse crate::ids::\{EffectId, InboxId, OperationId, SessionId\};\nuse crate::provider::ModelConfig;\nuse crate::session::SessionEntry;\nuse crate::store::\{\n    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,\n    SettledEffect, UsageRecord,\n\};\nuse crate::tool::ToolCall;\n\nuse super::ActiveOperation;\n\n.*?(?=/// Build the durable record of one staged transition\.)''',
    '''use crate::ids::{EffectId, InboxId, SessionId};
use crate::session::SessionEntry;
use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,
    SettledEffect, UsageRecord,
};

use super::ActiveOperation;

''',
)

runtime = "crates/ion-core/src/runtime/mod.rs"
replace_once(
    runtime,
    '''use persistence::{
    build_commit_request, compaction_from_input, model_step_from_input, tool_call_from_input,
};''',
    '''use persistence::build_commit_request;''',
)

recovery = "crates/ion-core/src/runtime/recovery.rs"
# Model-step recovery consumes the typed decoded plan rather than a historical
# tuple produced by a second JSON decoder.
sub_once(
    recovery,
    r'''                    let Some\(\(\n                        step,\n                        model,\n                        plan,\n                        persisted_snapshot_id,\n                        persisted_manifest_id,\n                        persisted_prefix_fingerprint,\n                        persisted_cache_expectation,\n                    \)\) = model_step_from_input\(&open\.effective_input\)\n                    else \{\n                        error!\(session = %self\.session_id, "pending model step lacks an exact model snapshot; fencing"\);\n                        self\.closed = true;\n                        return;\n                    \};''',
    '''                    let model_step = match open.decode() {
                        Ok(DurableEffect::ModelStep(plan)) => plan,
                        Ok(other) => {
                            error!(session = %self.session_id, effect = %open.id, ?other, "pending model-step state has the wrong durable effect kind; fencing");
                            self.closed = true;
                            return;
                        }
                        Err(err) => {
                            error!(session = %self.session_id, effect = %open.id, %err, "pending model step has an invalid durable effect; fencing");
                            self.closed = true;
                            return;
                        }
                    };
                    let step = model_step.step;
                    let model = model_step.model;
                    let plan = model_step.plan;
                    let persisted_snapshot_id = model_step.capability_snapshot_id;
                    let persisted_manifest_id = model_step.context_manifest_id;
                    let persisted_prefix_fingerprint = model_step.prefix_fingerprint;''',
)
replace_once(
    recovery,
    '''                    if snapshot_id != persisted_snapshot_id
                        || persisted_manifest_id.is_empty()
                        || persisted_prefix_fingerprint.is_empty()
                        || persisted_cache_expectation.is_empty()
                    {''',
    '''                    if snapshot_id != persisted_snapshot_id
                        || persisted_manifest_id.is_empty()
                        || persisted_prefix_fingerprint.is_empty()
                    {''',
)

sub_once(
    recovery,
    r'''                    let Some\(\(step, model, plan\)\) = compaction_from_input\(&open\.effective_input\)\n                    else \{\n                        error!\(session = %self\.session_id, "pending compaction lacks an exact model snapshot; fencing"\);\n                        self\.closed = true;\n                        return;\n                    \};''',
    '''                    let compaction = match open.decode() {
                        Ok(DurableEffect::Compaction(invocation)) => invocation,
                        Ok(other) => {
                            error!(session = %self.session_id, effect = %open.id, ?other, "pending compaction state has the wrong durable effect kind; fencing");
                            self.closed = true;
                            return;
                        }
                        Err(err) => {
                            error!(session = %self.session_id, effect = %open.id, %err, "pending compaction has an invalid durable effect; fencing");
                            self.closed = true;
                            return;
                        }
                    };
                    let step = compaction.step;
                    let model = compaction.model;
                    let plan = compaction.plan;''',
)

# Decode every pending tool effect once before branching on recovery policy.
replace_once(
    recovery,
    '''                    match open.recovery_class {
                        RecoveryClass::ReplaySafe => {''',
    '''                    let invocation = match open.decode() {
                        Ok(DurableEffect::Tool(invocation)) => invocation,
                        Ok(other) => {
                            error!(session = %self.session_id, effect = %open.id, ?other, "pending tool state has the wrong durable effect kind; fencing");
                            self.closed = true;
                            return;
                        }
                        Err(err) => {
                            error!(session = %self.session_id, effect = %open.id, %err, "pending tool has an invalid durable effect; fencing");
                            self.closed = true;
                            return;
                        }
                    };
                    match open.recovery_class {
                        RecoveryClass::ReplaySafe => {''',
)
sub_once(
    recovery,
    r'''                            let Some\(\(call, _invocation\)\) =\n                                tool_call_from_input\(operation_id, &open\.effective_input\)\n                            else \{\n                                error!\(session = %self\.session_id, effect = %open\.id, "pending replay-safe tool has an invalid durable invocation; fencing"\);\n                                self\.closed = true;\n                                return;\n                            \};''',
    '''                            let call = invocation.clone().into_call(operation_id);''',
)
sub_once(
    recovery,
    r'''                            let Some\(\(call, invocation\)\) =\n                                tool_call_from_input\(operation_id, &open\.effective_input\)\n                            else \{\n                                error!\(session = %self\.session_id, effect = %open\.id, "pending reconcilable tool has an invalid durable invocation; fencing"\);\n                                self\.closed = true;\n                                return;\n                            \};\n                            let evidence = invocation\n                                \.reconciliation\n                                \.clone\(\)\n                                \.unwrap_or\(serde_json::Value::Null\);''',
    '''                            let evidence = invocation
                                .reconciliation
                                .clone()
                                .unwrap_or(serde_json::Value::Null);
                            let call = invocation.into_call(operation_id);''',
)

replace_once(
    "DESIGN.md",
    '''8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery and multi-agent invariants. Runtime effect writers use the typed durable vocabulary; recovery/storage JSON decoding remains the next boundary to collapse.''',
    '''8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery and multi-agent invariants. Runtime effect writers and recovery consume the typed durable vocabulary through `EffectRecord`; SQLite's compact kind/JSON encoding is confined to that translation boundary. Remaining work is typed admission/evaluation coverage rather than parallel JSON interpretation.''',
)

print("Step 8 typed recovery migration applied")
