from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}")
    file.write_text(text.replace(old, new, 1))


runtime = "crates/ion-core/src/runtime/mod.rs"
replace_once(
    runtime,
    '''use crate::error::{CommandError, RuntimeError};
use crate::ids::{''',
    '''use crate::effect::{
    CacheExpectation, CompactionInvocation, DurableEffect, ModelStepPlan, ToolInvocation,
};
use crate::error::{CommandError, RuntimeError};
use crate::harness::HarnessProfile;
use crate::ids::{''',
)
replace_once(
    runtime,
    '''    fn cache_expectation(
        &self,
        operation_id: OperationId,
        model: &ModelConfig,
        prefix_fingerprint: &str,
    ) -> &'static str {
        if !model.capabilities.prompt_cache {
            return "unsupported";
        }
        match self
            .operation_lane_live(operation_id)
            .expect("resident operation has an owning lane")
            .last_prefix_fingerprint
            .as_deref()
        {
            None => "cold_start",
            Some(previous) if previous == prefix_fingerprint => "prefix_reuse_expected",
            Some(_) => "prefix_changed",
        }
    }''',
    '''    fn cache_expectation(
        &self,
        operation_id: OperationId,
        model: &ModelConfig,
        prefix_fingerprint: &str,
    ) -> CacheExpectation {
        if !model.capabilities.prompt_cache {
            return CacheExpectation::Unsupported;
        }
        match self
            .operation_lane_live(operation_id)
            .expect("resident operation has an owning lane")
            .last_prefix_fingerprint
            .as_deref()
        {
            None => CacheExpectation::ColdStart,
            Some(previous) if previous == prefix_fingerprint => {
                CacheExpectation::PrefixReuseExpected
            }
            Some(_) => CacheExpectation::PrefixChanged,
        }
    }''',
)
replace_once(
    runtime,
    '''        let effect = EffectRecord {
            id: EffectId::generate(),
            kind: "compaction".to_owned(),
            recovery_class: RecoveryClass::ReplaySafe,
            effective_input: serde_json::json!({
                "step": self.live_mut(operation_id).expect("main operation residency exists").model_step + 1,
                "model": model,
                "plan": plan
            }),
            attempt: 1,
        };''',
    '''        let effect = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::Compaction(CompactionInvocation {
                step: self
                    .live_mut(operation_id)
                    .expect("main operation residency exists")
                    .model_step
                    + 1,
                model: model.clone(),
                plan: plan.clone(),
                harness_profile: HarnessProfile::default_v1(),
            }),
            RecoveryClass::ReplaySafe,
            1,
        );''',
)
replace_once(
    runtime,
    '''        let effect = EffectRecord {
            id: EffectId::generate(),
            kind: "model_step".to_owned(),
            recovery_class: RecoveryClass::ReplaySafe,
            effective_input: serde_json::json!({
                "step": self.live_mut(operation_id).expect("main operation residency exists").model_step + 1,
                "model": model,
                "plan": plan,
                "capability_snapshot_id": capability_snapshot.id,
                "context_manifest_id": context_manifest.id,
                "prefix_fingerprint": prefix_fingerprint,
                "cache_expectation": cache_expectation
            }),
            attempt: 1,
        };''',
    '''        let effect = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::ModelStep(ModelStepPlan {
                step: self
                    .live_mut(operation_id)
                    .expect("main operation residency exists")
                    .model_step
                    + 1,
                model: model.clone(),
                plan: plan.clone(),
                capability_snapshot_id: capability_snapshot.id.clone(),
                context_manifest_id: context_manifest.id.clone(),
                harness_profile: HarnessProfile::default_v1(),
                prefix_fingerprint: prefix_fingerprint.clone(),
                cache_expectation,
            }),
            RecoveryClass::ReplaySafe,
            1,
        );''',
)
replace_once(
    runtime,
    '''        let effect = EffectRecord {
            id: EffectId::generate(),
            kind: format!("tool:{}", call.name),
            recovery_class,
            effective_input: serde_json::json!({
                "tool": call.name,
                "arguments": call.arguments,
                "call_id": call.call_id,
                "canonical": canonical,
                "reconciliation": evidence,
            }),
            attempt: 1,
        };''',
    '''        let effect = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::Tool(ToolInvocation {
                tool: call.name.clone(),
                arguments: call.arguments.clone(),
                call_id: call.call_id,
                canonical,
                reconciliation: evidence.clone(),
            }),
            recovery_class,
            1,
        );''',
)
replace_once(
    runtime,
    '''            let reconciliation = self
                .active(operation_id)
                .and_then(|active| active.open_effect.as_ref())
                .and_then(|effect| effect.effective_input.get("reconciliation"))
                .cloned();
            let effect_id = self
                .active(operation_id)
                .and_then(|active| active.open_effect.as_ref().map(|effect| effect.id));
            self.spawn_tool_effect(effect_id, call, reconciliation, step_tools);''',
    '''            let effect_id = self
                .active(operation_id)
                .and_then(|active| active.open_effect.as_ref().map(|effect| effect.id));
            self.spawn_tool_effect(effect_id, call, evidence, step_tools);''',
)

replace_once(
    "crates/ion-core/src/runtime/effects.rs",
    '''        let effect = EffectRecord {
            id: EffectId::generate(),
            kind: "compaction".to_owned(),
            recovery_class: RecoveryClass::ReplaySafe,
            effective_input: serde_json::json!({
                "step": self.live_mut(operation_id).expect("main operation residency exists").model_step + 1,
                "model": model,
                "plan": plan
            }),
            attempt: 1,
        };''',
    '''        let effect = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::Compaction(CompactionInvocation {
                step: self
                    .live_mut(operation_id)
                    .expect("main operation residency exists")
                    .model_step
                    + 1,
                model: model.clone(),
                plan: plan.clone(),
                harness_profile: HarnessProfile::default_v1(),
            }),
            RecoveryClass::ReplaySafe,
            1,
        );''',
)

replace_once(
    "crates/ion-core/src/tests/structural_scope_recovery.rs",
    '''    let loaded = store.load(session_id).await.expect("load");
    let frozen = &loaded.operations[0].capability_snapshot;''',
    '''    let loaded = store.load(session_id).await.expect("load");
    let open = loaded.operations[0]
        .latest
        .1
        .open_effect
        .as_ref()
        .expect("pending model effect");
    let DurableEffect::ModelStep(model_step) = open.decode().expect("typed model effect") else {
        panic!("expected model-step effect");
    };
    assert_eq!(model_step.harness_profile, HarnessProfile::default_v1());
    assert_eq!(
        open.effective_input
            .get("harness_profile")
            .and_then(|profile| profile.get("id"))
            .and_then(serde_json::Value::as_str),
        Some(DEFAULT_HARNESS_PROFILE_ID),
        "new runtime writers persist the harness profile explicitly"
    );
    let frozen = &loaded.operations[0].capability_snapshot;''',
)

replace_once(
    "DESIGN.md",
    '''8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery and multi-agent invariants.''',
    '''8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery and multi-agent invariants. Runtime effect writers use the typed durable vocabulary; recovery/storage JSON decoding remains the next boundary to collapse.''',
)

print("Step 8 typed effect writer migration applied")
