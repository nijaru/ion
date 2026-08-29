from pathlib import Path

# ion-core's public crate description should describe the established ownership
# model, not a migration that has already completed.
lib = Path("crates/ion-core/src/lib.rs")
text = lib.read_text()
old = """//! Authoritative session state has one mutation owner; provider, tool, and
//! agent effects execute concurrently outside that mutation line and become
//! authoritative only through durable transitions. The current implementation
//! is migrating from one linear session operation to a tree/lane substrate.
"""
new = """//! Authoritative session state has one mutation owner; provider, tool, and
//! agent effects execute concurrently outside that mutation line and become
//! authoritative only through durable transitions. Durable conversation state
//! is a tree addressed through independent lane cursors.
"""
assert text.count(old) == 1, "crate ownership description changed"
text = text.replace(old, new, 1)
old = """pub use effect::{
    CacheExpectation, CompactionInvocation, DurableEffect, ModelStepPlan, ToolInvocation,
};
"""
assert text.count(old) == 1, "effect export block changed"
text = text.replace(old, "", 1)
old = "pub use harness::{DEFAULT_HARNESS_PROFILE_ID, HarnessProfile};\n"
assert text.count(old) == 1, "harness export block changed"
text = text.replace(old, "", 1)
old = """pub use store::{
    CheckpointPayload, EffectRecord, EntryRecord, LoadedOperation, LoadedSession, SessionRecord,
    SessionStore, StoreError, default_db_path,
};
"""
new = """pub use store::{EntryRecord, LoadedSession, SessionRecord, SessionStore, StoreError, default_db_path};
"""
assert text.count(old) == 1, "store export block changed"
lib.write_text(text.replace(old, new, 1))

# Durable effect construction/decoding is the runtime/store translation
# boundary. It is not a host/frontend contract.
effect = Path("crates/ion-core/src/effect.rs")
text = effect.read_text()
for old, new in [
    ("pub enum CacheExpectation", "pub(crate) enum CacheExpectation"),
    ("pub struct ModelStepPlan", "pub(crate) struct ModelStepPlan"),
    ("pub struct CompactionInvocation", "pub(crate) struct CompactionInvocation"),
    ("pub struct ToolInvocation", "pub(crate) struct ToolInvocation"),
    ("pub enum DurableEffect", "pub(crate) enum DurableEffect"),
    ("    pub const fn as_str", "    pub(crate) const fn as_str"),
    ("    pub fn into_call", "    pub(crate) fn into_call"),
    ("    pub fn new(\n", "    pub(crate) fn new(\n"),
    ("    pub fn decode(&self)", "    pub(crate) fn decode(&self)"),
    ("    pub fn next_attempt(&self)", "    pub(crate) fn next_attempt(&self)"),
]:
    assert text.count(old) == 1, f"effect visibility target changed: {old}"
    text = text.replace(old, new, 1)
effect.write_text(text)

harness = Path("crates/ion-core/src/harness.rs")
text = harness.read_text()
for old, new in [
    ("pub const DEFAULT_HARNESS_PROFILE_ID", "pub(crate) const DEFAULT_HARNESS_PROFILE_ID"),
    ("pub struct HarnessProfile", "pub(crate) struct HarnessProfile"),
    ("    pub id: String,", "    pub(crate) id: String,"),
    ("    pub fn default_v1()", "    pub(crate) fn default_v1()"),
    ("    pub fn is_supported(&self)", "    pub(crate) fn is_supported(&self)"),
]:
    assert text.count(old) == 1, f"harness visibility target changed: {old}"
    text = text.replace(old, new, 1)
harness.write_text(text)

# SessionStore::load remains the public semantic readback contract. Recovery,
# checkpoint and auxiliary effect state are runtime-owned fields of that loaded
# aggregate; the public host only needs persisted session metadata + entries.
store = Path("crates/ion-core/src/store/mod.rs")
text = store.read_text()
for old, new in [
    ("pub struct CheckpointPayload", "pub(crate) struct CheckpointPayload"),
    ("pub struct CheckpointRecord", "pub(crate) struct CheckpointRecord"),
    ("pub struct EffectRecord", "pub(crate) struct EffectRecord"),
    ("pub struct AssistantFrame", "pub(crate) struct AssistantFrame"),
    ("pub struct ToolProgressCheckpoint", "pub(crate) struct ToolProgressCheckpoint"),
    ("pub struct SettledEffect", "pub(crate) struct SettledEffect"),
    ("pub struct InboxRecord", "pub(crate) struct InboxRecord"),
    ("pub enum InboxStatus", "pub(crate) enum InboxStatus"),
    ("pub struct CommitRequest", "pub(crate) struct CommitRequest"),
    ("pub struct UsageRecord", "pub(crate) struct UsageRecord"),
    ("pub struct UsageRow", "pub(crate) struct UsageRow"),
    ("pub struct LoadedOperation", "pub(crate) struct LoadedOperation"),
    ("    pub operations: Vec<LoadedOperation>,", "    pub(crate) operations: Vec<LoadedOperation>,"),
    ("    pub assistant_frames: Vec<AssistantFrame>,", "    pub(crate) assistant_frames: Vec<AssistantFrame>,"),
    ("    pub tool_progress: Vec<ToolProgressCheckpoint>,", "    pub(crate) tool_progress: Vec<ToolProgressCheckpoint>,"),
    ("    pub latest_usage: Option<UsageRow>,", "    pub(crate) latest_usage: Option<UsageRow>,"),
]:
    assert text.count(old) == 1, f"store visibility target changed: {old}"
    text = text.replace(old, new, 1)
old = """    Usage {
        session_id: SessionId,
        reply: oneshot::Sender<Result<Vec<UsageRow>, StoreError>>,
    },
"""
new = """    #[cfg(test)]
    Usage {
        session_id: SessionId,
        reply: oneshot::Sender<Result<Vec<UsageRow>, StoreError>>,
    },
"""
assert text.count(old) == 1, "usage command changed"
text = text.replace(old, new, 1)
old = """    /// Token usage rows recorded for one session (DESIGN.md §27.2).
    pub async fn usage(&self, session_id: SessionId) -> Result<Vec<UsageRow>, StoreError> {
        self.request(|reply| StoreCommand::Usage { session_id, reply })
            .await
    }
"""
new = """    /// Unit-test probe for the durable usage ledger (DESIGN.md §27.2).
    #[cfg(test)]
    pub(crate) async fn usage(&self, session_id: SessionId) -> Result<Vec<UsageRow>, StoreError> {
        self.request(|reply| StoreCommand::Usage { session_id, reply })
            .await
    }
"""
assert text.count(old) == 1, "usage method changed"
store.write_text(text.replace(old, new, 1))

sql = Path("crates/ion-core/src/store/sql.rs")
text = sql.read_text()
old = """        StoreCommand::Usage { session_id, reply } => {
            let _ = reply.send(usage_rows(connection, session_id));
        }
"""
new = """        #[cfg(test)]
        StoreCommand::Usage { session_id, reply } => {
            let _ = reply.send(usage_rows(connection, session_id));
        }
"""
assert text.count(old) == 1, "usage command handler changed"
text = text.replace(old, new, 1)
old = "\nfn usage_rows("
new = "\n#[cfg(test)]\nfn usage_rows("
assert text.count(old) == 1, "usage_rows function changed"
sql.write_text(text.replace(old, new, 1))

scope_test = Path("crates/ion-core/src/tests/structural_scope_recovery.rs")
text = scope_test.read_text()
old = "use crate::{DEFAULT_HARNESS_PROFILE_ID, DurableEffect, HarnessProfile};\n"
new = """use crate::effect::DurableEffect;
use crate::harness::{DEFAULT_HARNESS_PROFILE_ID, HarnessProfile};
"""
assert text.count(old) == 1, "structural recovery internal imports changed"
scope_test.write_text(text.replace(old, new, 1))

design = Path("DESIGN.md")
text = design.read_text()
old = "9. Shrink/rename public Rust API and remove dead migration scaffolding now that ownership is stable. Session topology no longer aliases the operation reducer, and low-level store mutation primitives are crate-owned behind the runtime; public store surface is for host lifecycle/readback rather than constructing transactions. Test-only store probes are compiled only for unit tests. Keep frontend-visible state/history contracts public and do not rename coherent runtime/session concepts speculatively."
new = "9. Public Rust API cleanup is established for current owners: dead runtime/reducer migration seams are gone; store mutation, recovery/checkpoint/effect encoding, harness-profile identity, and raw ledger probes are crate-owned; host/frontend surface retains runtime control plus semantic session/history readback. No coherent runtime/session concepts were renamed speculatively."
assert text.count(old) == 1, "Step 9 design text changed"
design.write_text(text.replace(old, new, 1))
