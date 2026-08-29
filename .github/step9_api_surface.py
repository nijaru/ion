from pathlib import Path
import re

runtime = Path("crates/ion-core/src/runtime/mod.rs")
text = runtime.read_text()
pattern = re.compile(
    r"/// Command sender for the process runtime \(DESIGN\.md §8\.1\).*?"
    r"fn command_send_error\(err: mpsc::error::TrySendError<SessionCommand>\) -> CommandError \{\n"
    r"    match err \{\n"
    r"        mpsc::error::TrySendError::Full\(_\) => CommandError::QueueSaturated,\n"
    r"        mpsc::error::TrySendError::Closed\(_\) => CommandError::Closed,\n"
    r"    \}\n"
    r"\}\n\n"
    r"(?=#\[cfg\(test\)\]\npub\(crate\) struct SaturatedHandle)",
    re.S,
)
replacement = """fn command_send_error(err: mpsc::error::TrySendError<SessionCommand>) -> CommandError {
    match err {
        mpsc::error::TrySendError::Full(_) => CommandError::QueueSaturated,
        mpsc::error::TrySendError::Closed(_) => CommandError::Closed,
    }
}

"""
text, count = pattern.subn(replacement, text, count=1)
assert count == 1, f"expected one RuntimeHandle block, got {count}"
text = text.replace("    handle: RuntimeHandle,\n", "    handle: SessionHandle,\n", 1)
text = text.replace("        let handle = RuntimeHandle { tx };\n", "        let handle = SessionHandle { tx };\n", 1)
text = text.replace(
    "    pub(crate) fn handle(&self) -> &RuntimeHandle {\n",
    "    pub(crate) fn handle(&self) -> &SessionHandle {\n",
    1,
)
text = text.replace("impl RuntimeHandle {\n    fn fill_queue", "impl SessionHandle {\n    fn fill_queue", 1)
assert "RuntimeHandle" not in text, "RuntimeHandle remains in runtime module"
runtime.write_text(text)

lib = Path("crates/ion-core/src/lib.rs")
text = lib.read_text()
text = text.replace(
    "    OperationStatus, PendingTool, Runtime, RuntimeBudget, RuntimeEvent, RuntimeHandle,\n    SessionHandle, SessionSnapshot,\n",
    "    OperationStatus, PendingTool, Runtime, RuntimeBudget, RuntimeEvent, SessionHandle,\n    SessionSnapshot,\n",
    1,
)
text = text.replace(
    "    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,\n",
    "    CheckpointPayload, CheckpointRecord, EffectRecord, EntryRecord, InboxRecord,\n",
    1,
)
assert "RuntimeHandle" not in text, "RuntimeHandle remains publicly re-exported"
lib.write_text(text)

design = Path("DESIGN.md")
text = design.read_text()
old8 = "8. Finish typed tool/effect admission boundaries and expand evaluation around recovery and multi-agent invariants. Runtime effect writers and recovery consume the typed durable vocabulary through `EffectRecord`; ordinary and approved tool calls share one typed post-policy admission preparation, so canonical/recovery/reconciliation/denial state reaches the durable commit boundary as one coherent value. SQLite's compact kind/JSON encoding remains confined to the effect translation boundary."
new8 = "8. Typed tool/effect admission and recovery boundaries are established for current owners. Runtime writers and recovery consume `EffectRecord`, tool admission reaches the durable boundary as one coherent typed value, and recovery/multi-agent invariants cover structural-scope narrowing without a separate unowned eval crate."
assert old8 in text, "Step 8 design text changed"
text = text.replace(old8, new8, 1)
old9 = "9. Shrink/rename public Rust API and remove dead migration scaffolding once ownership is stable."
new9 = "9. Shrink/rename public Rust API and remove dead migration scaffolding now that ownership is stable. Begin with usage-proven accidental surface; do not rename coherent runtime/session concepts speculatively."
assert old9 in text, "Step 9 design text changed"
text = text.replace(old9, new9, 1)
design.write_text(text)
