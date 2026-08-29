from pathlib import Path

store = Path("crates/ion-core/src/store/mod.rs")
text = store.read_text()
replacements = {
    "    pub async fn create_session(&self, record: SessionRecord) -> Result<(), StoreError> {\n":
        "    pub(crate) async fn create_session(&self, record: SessionRecord) -> Result<(), StoreError> {\n",
    "    pub async fn create_lane(\n": "    pub(crate) async fn create_lane(\n",
    "    pub async fn begin_operation(\n": "    pub(crate) async fn begin_operation(\n",
    "    pub async fn commit(&self, request: CommitRequest) -> Result<(), StoreError> {\n":
        "    pub(crate) async fn commit(&self, request: CommitRequest) -> Result<(), StoreError> {\n",
    "    pub async fn append_entry(\n": "    pub(crate) async fn append_entry(\n",
    "    pub async fn usage(&self, session_id: SessionId) -> Result<Vec<UsageRow>, StoreError> {\n":
        "    pub(crate) async fn usage(&self, session_id: SessionId) -> Result<Vec<UsageRow>, StoreError> {\n",
    "    pub async fn upsert_assistant_frame(&self, frame: AssistantFrame) -> Result<(), StoreError> {\n":
        "    pub(crate) async fn upsert_assistant_frame(&self, frame: AssistantFrame) -> Result<(), StoreError> {\n",
    "    pub async fn upsert_tool_progress(\n": "    pub(crate) async fn upsert_tool_progress(\n",
    "    pub fn fail_next_write(&self) {\n": "    pub(crate) fn fail_next_write(&self) {\n",
}
for old, new in replacements.items():
    assert text.count(old) == 1, f"store method signature changed: {old.strip()}"
    text = text.replace(old, new, 1)
store.write_text(text)

lib = Path("crates/ion-core/src/lib.rs")
text = lib.read_text()
old_operation = "pub use operation::{InboxKind, OperationOutcome, OperationState, SessionEntry};\n"
new_operation = "pub use operation::{OperationOutcome, OperationState, SessionEntry};\n"
assert old_operation in text, "operation export block changed"
text = text.replace(old_operation, new_operation, 1)
old_store = """pub use store::{
    CheckpointPayload, CheckpointRecord, EffectRecord, EntryRecord, InboxRecord, InboxStatus,
    LoadedOperation, LoadedSession, SessionRecord, SessionStore, StoreError, default_db_path,
};
"""
new_store = """pub use store::{
    CheckpointPayload, EffectRecord, EntryRecord, LoadedOperation, LoadedSession, SessionRecord,
    SessionStore, StoreError, default_db_path,
};
"""
assert old_store in text, "store root export block changed"
text = text.replace(old_store, new_store, 1)
lib.write_text(text)

design = Path("DESIGN.md")
text = design.read_text()
old = "9. Shrink/rename public Rust API and remove dead migration scaffolding now that ownership is stable. Accidental runtime/store exports are being removed first; session topology no longer aliases the operation reducer. Keep frontend-visible state/history contracts public and do not rename coherent runtime/session concepts speculatively."
new = "9. Shrink/rename public Rust API and remove dead migration scaffolding now that ownership is stable. Session topology no longer aliases the operation reducer, and low-level store mutation primitives are crate-owned behind the runtime; public store surface is for host lifecycle/readback rather than constructing transactions. Keep frontend-visible state/history contracts public and do not rename coherent runtime/session concepts speculatively."
assert old in text, "Step 9 design text changed"
design.write_text(text.replace(old, new, 1))
