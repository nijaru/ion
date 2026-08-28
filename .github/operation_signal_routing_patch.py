from pathlib import Path


def replace_once(path: str, old: str, new: str, label: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    p.write_text(text.replace(old, new, 1))


runtime = "crates/ion-core/src/runtime/mod.rs"
replace_once(
    runtime,
    '''enum ToolSignal {\n    Progress {\n        effect_id: EffectId,\n        call_id: u64,\n        output: String,\n    },\n    Settled {\n        effect_id: EffectId,\n        result: ToolResult,\n    },\n}\n''',
    '''enum ToolSignal {\n    Progress {\n        operation_id: OperationId,\n        effect_id: EffectId,\n        call_id: u64,\n        output: String,\n    },\n    Settled {\n        operation_id: OperationId,\n        effect_id: EffectId,\n        result: ToolResult,\n    },\n}\n''',
    "tool signals carry operation identity",
)
replace_once(
    runtime,
    '''            let _ = self.tool_tx.try_send(ToolSignal::Settled {\n                effect_id,\n                result: ToolResult::Err {\n''',
    '''            let _ = self.tool_tx.try_send(ToolSignal::Settled {\n                operation_id: call.operation_id,\n                effect_id,\n                result: ToolResult::Err {\n''',
    "synthetic denial settlement routes by operation",
)

effects = "crates/ion-core/src/runtime/effects.rs"
replace_once(
    effects,
    '''        let tool_tx = self.tool_tx.clone();\n        let (progress_tx, mut progress_rx) = mpsc::channel::<ToolProgress>(8);\n        let ToolCall {\n            call_id,\n            name,\n            arguments,\n            ..\n        } = call;\n''',
    '''        let tool_tx = self.tool_tx.clone();\n        let (progress_tx, mut progress_rx) = mpsc::channel::<ToolProgress>(8);\n        let ToolCall {\n            operation_id,\n            call_id,\n            name,\n            arguments,\n        } = call;\n''',
    "tool spawn retains operation identity",
)
replace_once(
    effects,
    '''                        .send(ToolSignal::Progress {\n                            effect_id,\n                            call_id,\n                            output: progress.output,\n                        })\n''',
    '''                        .send(ToolSignal::Progress {\n                            operation_id,\n                            effect_id,\n                            call_id,\n                            output: progress.output,\n                        })\n''',
    "tool progress routes by operation",
)
replace_once(
    effects,
    '''            let _ = tool_tx\n                .send(ToolSignal::Settled { effect_id, result })\n                .await;\n''',
    '''            let _ = tool_tx\n                .send(ToolSignal::Settled {\n                    operation_id,\n                    effect_id,\n                    result,\n                })\n                .await;\n''',
    "tool settlement routes by operation",
)
replace_once(
    effects,
    '''        let (effect_id, result) = match settlement {\n            ToolSignal::Settled { effect_id, result } => (effect_id, result),\n            ToolSignal::Progress {\n                effect_id,\n                call_id,\n                output,\n            } => {\n                let Some(active) = self.operation.as_ref() else {\n                    return;\n                };\n                if active.open_effect.as_ref().map(|effect| effect.id) != Some(effect_id) {\n                    return;\n                }\n                let operation_id = active.machine.operation_id();\n''',
    '''        let (operation_id, effect_id, result) = match settlement {\n            ToolSignal::Settled {\n                operation_id,\n                effect_id,\n                result,\n            } => (operation_id, effect_id, result),\n            ToolSignal::Progress {\n                operation_id,\n                effect_id,\n                call_id,\n                output,\n            } => {\n                let Some(active) = self.operation.as_ref() else {\n                    return;\n                };\n                if active.machine.operation_id() != operation_id\n                    || active.open_effect.as_ref().map(|effect| effect.id) != Some(effect_id)\n                {\n                    debug!(%operation_id, %effect_id, "dropped stale tool progress");\n                    return;\n                }\n''',
    "tool handler dispatch identity",
)
replace_once(
    effects,
    '''        let expected = self\n            .operation\n            .as_ref()\n            .and_then(|active| active.open_effect.as_ref().map(|e| e.id));\n        if expected != Some(effect_id) {\n            // Stale or unknown tool result: a typed diagnostic, never a\n            // panic and never a state change.\n            debug!(?effect_id, ?expected, "dropped stale tool settlement");\n            return;\n        }\n''',
    '''        let expected = self.operation.as_ref().and_then(|active| {\n            (active.machine.operation_id() == operation_id)\n                .then(|| active.open_effect.as_ref().map(|effect| effect.id))\n                .flatten()\n        });\n        if expected != Some(effect_id) {\n            // Stale or unknown tool result: operation and effect identity\n            // must both match before the session writer mutates state.\n            debug!(%operation_id, %effect_id, ?expected, "dropped stale tool settlement");\n            return;\n        }\n''',
    "tool settlement validates operation identity",
)
