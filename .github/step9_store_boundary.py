from pathlib import Path

store = Path("crates/ion-core/src/store/mod.rs")
text = store.read_text()

# Runtime-owned mutation primitives are crate-visible. Readback stays public.
for old, new in {
    "    pub async fn create_session(&self, record: SessionRecord) -> Result<(), StoreError> {\n":
        "    pub(crate) async fn create_session(&self, record: SessionRecord) -> Result<(), StoreError> {\n",
    "    pub async fn begin_operation(\n": "    pub(crate) async fn begin_operation(\n",
    "    pub async fn commit(&self, request: CommitRequest) -> Result<(), StoreError> {\n":
        "    pub(crate) async fn commit(&self, request: CommitRequest) -> Result<(), StoreError> {\n",
    "    pub async fn upsert_assistant_frame(&self, frame: AssistantFrame) -> Result<(), StoreError> {\n":
        "    pub(crate) async fn upsert_assistant_frame(&self, frame: AssistantFrame) -> Result<(), StoreError> {\n",
    "    pub async fn upsert_tool_progress(\n": "    pub(crate) async fn upsert_tool_progress(\n",
}.items():
    assert text.count(old) == 1, f"store method signature changed: {old.strip()}"
    text = text.replace(old, new, 1)

# This convenience wrapper has no caller; runtime uses create_lane_with_config.
old_create_lane = """    /// Create one durable lane anchored at an existing conversation leaf.
    /// The lane must be idle at creation; operations and pending input are
    /// admitted by their own transactions after topology exists.
    pub async fn create_lane(
        &self,
        session_id: SessionId,
        lane_name: impl Into<String>,
        source_leaf: Option<EntryId>,
        model_ref: impl Into<String>,
    ) -> Result<(), StoreError> {
        self.create_lane_with_config(
            session_id,
            lane_name,
            source_leaf,
            crate::session::lane::Config::new(model_ref),
        )
        .await
    }

"""
assert text.count(old_create_lane) == 1, "create_lane wrapper changed"
text = text.replace(old_create_lane, "", 1)

# Direct entry append exists only to seed store-level unit tests. Compile the
# whole command path only in ion-core test builds instead of exposing it.
old_variant = """    AppendEntry {
        session_id: SessionId,
        lane_name: String,
        entry: EntryRecord,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
"""
new_variant = """    #[cfg(test)]
    AppendEntry {
        session_id: SessionId,
        lane_name: String,
        entry: EntryRecord,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
"""
assert text.count(old_variant) == 1, "AppendEntry command changed"
text = text.replace(old_variant, new_variant, 1)
old_append = """    /// Append one semantic conversation entry. The session runtime assigns seq.
    pub async fn append_entry(
        &self,
        session_id: SessionId,
        lane_name: impl Into<String>,
        entry: EntryRecord,
    ) -> Result<(), StoreError> {
        let lane_name = lane_name.into();
        self.request(|reply| StoreCommand::AppendEntry {
            session_id,
            lane_name,
            entry,
            reply,
        })
        .await
    }

"""
new_append = """    /// Test helper for seeding semantic history without a live runtime.
    #[cfg(test)]
    pub(crate) async fn append_entry(
        &self,
        session_id: SessionId,
        lane_name: impl Into<String>,
        entry: EntryRecord,
    ) -> Result<(), StoreError> {
        let lane_name = lane_name.into();
        self.request(|reply| StoreCommand::AppendEntry {
            session_id,
            lane_name,
            entry,
            reply,
        })
        .await
    }

"""
assert text.count(old_append) == 1, "append_entry method changed"
text = text.replace(old_append, new_append, 1)

# Failure injection is also unit-test-only. The store thread still receives a
# false flag in production, but SessionStore does not retain/expose the hook.
old_field = """    /// Test hook: fail the next mutating command (DESIGN.md §30.5).
    fail_next_write: Arc<AtomicBool>,
"""
new_field = """    /// Test hook: fail the next mutating command (DESIGN.md §30.5).
    #[cfg(test)]
    fail_next_write: Arc<AtomicBool>,
"""
assert text.count(old_field) == 1, "fail_next_write field changed"
text = text.replace(old_field, new_field, 1)
old_init = """            startup_notice,
            fail_next_write,
            join: Arc::new(Mutex::new(Some(join))),
"""
new_init = """            startup_notice,
            #[cfg(test)]
            fail_next_write,
            join: Arc::new(Mutex::new(Some(join))),
"""
assert text.count(old_init) == 1, "store init changed"
text = text.replace(old_init, new_init, 1)
old_fail = """    /// Test hook (DESIGN.md §30.5): the next mutating command fails
    /// visibly and nothing is written.
    pub fn fail_next_write(&self) {
        self.fail_next_write.store(true, Ordering::SeqCst);
    }
"""
new_fail = """    /// Test hook (DESIGN.md §30.5): the next mutating command fails
    /// visibly and nothing is written.
    #[cfg(test)]
    pub(crate) fn fail_next_write(&self) {
        self.fail_next_write.store(true, Ordering::SeqCst);
    }
"""
assert text.count(old_fail) == 1, "fail_next_write method changed"
text = text.replace(old_fail, new_fail, 1)
store.write_text(text)

sql = Path("crates/ion-core/src/store/sql.rs")
text = sql.read_text()
old_arm = """        StoreCommand::AppendEntry {
            session_id,
            lane_name,
            entry,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                append_entry(connection, session_id, &lane_name, &entry).map_err(StoreError::from)
            }));
        }
"""
new_arm = """        #[cfg(test)]
        StoreCommand::AppendEntry {
            session_id,
            lane_name,
            entry,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                append_entry(connection, session_id, &lane_name, &entry).map_err(StoreError::from)
            }));
        }
"""
assert text.count(old_arm) == 1, "AppendEntry handler changed"
text = text.replace(old_arm, new_arm, 1)
old_fn = """fn append_entry(
    connection: &mut Connection,
    session_id: SessionId,
    lane_name: &str,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
"""
new_fn = """#[cfg(test)]
fn append_entry(
    connection: &mut Connection,
    session_id: SessionId,
    lane_name: &str,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
"""
assert text.count(old_fn) == 1, "append_entry SQL function changed"
text = text.replace(old_fn, new_fn, 1)
sql.write_text(text)

# Tests inside ion-core should name the now-internal record enum at its owner.
reconcile = Path("crates/ion-core/src/tests/reconcile.rs")
text = reconcile.read_text()
assert text.count("crate::InboxStatus::Applied") == 1, "InboxStatus test path changed"
reconcile.write_text(text.replace("crate::InboxStatus::Applied", "crate::store::InboxStatus::Applied", 1))

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
new = "9. Shrink/rename public Rust API and remove dead migration scaffolding now that ownership is stable. Session topology no longer aliases the operation reducer, and low-level store mutation primitives are crate-owned behind the runtime; public store surface is for host lifecycle/readback rather than constructing transactions. Test-only store probes are compiled only for unit tests. Keep frontend-visible state/history contracts public and do not rename coherent runtime/session concepts speculatively."
assert old in text, "Step 9 design text changed"
design.write_text(text.replace(old, new, 1))
