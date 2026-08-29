from pathlib import Path

runtime = Path("crates/ion-core/src/runtime/mod.rs")
text = runtime.read_text()
old = """/// Command sender for the process runtime (DESIGN.md §8.1). Session
/// commands live on [`SessionHandle`]; one-shot callers reach the sole
/// session through [`Runtime::session`].
#[derive(Clone)]
pub struct RuntimeHandle {
    tx: mpsc::Sender<SessionCommand>,
}

impl fmt::Debug for RuntimeHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeHandle").finish_non_exhaustive()
    }
}

impl RuntimeHandle {
    /// Close the runtime's sessions and shut down (DESIGN.md §25.2).
    pub async fn shutdown(&self) -> Result<(), CommandError> {
        self.request(|reply| SessionCommand::Close { reply }).await
    }

    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, CommandError>>) -> SessionCommand,
    ) -> Result<T, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx.try_send(build(reply)).map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }
}

"""
assert text.count(old) == 1, "RuntimeHandle block changed"
text = text.replace(old, "", 1)
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
old_runtime_export = """pub use runtime::{
    EventSubscription, HostedRuntimeConfig, IndeterminateWarning, LiveOperationState,
    OperationStatus, PendingTool, Runtime, RuntimeBudget, RuntimeEvent, RuntimeHandle,
    SessionHandle, SessionSnapshot,
};
"""
new_runtime_export = """pub use runtime::{
    EventSubscription, HostedRuntimeConfig, IndeterminateWarning, LiveOperationState,
    OperationStatus, PendingTool, Runtime, RuntimeBudget, RuntimeEvent, SessionHandle,
    SessionSnapshot,
};
"""
assert old_runtime_export in text, "runtime export block changed"
text = text.replace(old_runtime_export, new_runtime_export, 1)
old_store_export = """pub use store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,
    InboxStatus, LoadedOperation, LoadedSession, SessionRecord, SessionStore, StoreError,
    default_db_path,
};
"""
new_store_export = """pub use store::{
    CheckpointPayload, CheckpointRecord, EffectRecord, EntryRecord, InboxRecord, InboxStatus,
    LoadedOperation, LoadedSession, SessionRecord, SessionStore, StoreError, default_db_path,
};
"""
assert old_store_export in text, "store export block changed"
text = text.replace(old_store_export, new_store_export, 1)
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
