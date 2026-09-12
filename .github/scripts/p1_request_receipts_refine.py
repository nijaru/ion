from pathlib import Path
import subprocess
import textwrap


def replace(path: str, old: str, new: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    found = text.count(old)
    if found != count:
        raise SystemExit(
            f"{path}: expected {count} exact matches, found {found}: {old[:180]!r}"
        )
    p.write_text(text.replace(old, new, count))


# Reuse the already-reviewed base source transformation rather than carrying a
# second copy of the large patch. This helper is temporary and deleted after
# the branch is validated and promoted.
workflow = Path(".github/workflows/p1-request-receipts-patch.yml").read_text()
start = "          python3 <<'PY'\n"
end = "\n          PY\n"
body = workflow.split(start, 1)[1].split(end, 1)[0]
base_patch = Path("/tmp/base_patch.py")
base_patch.write_text(textwrap.dedent(body))
subprocess.run(["python3", str(base_patch)], check=True)

store = "crates/ion-core/src/store/mod.rs"
replace(
    store,
    """#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BeginOperationResult {
    Accepted(Option<InputReceipt>),
    Duplicate(InputReceipt),
}
""",
    """#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BeginOperationResult {
    Accepted(Option<InputReceipt>),
    Duplicate(InputReceipt),
}

pub(crate) struct BeginOperationRequest {
    pub(crate) session_id: SessionId,
    pub(crate) lane_name: String,
    pub(crate) operation_id: OperationId,
    pub(crate) root_inbox: InboxRecord,
    pub(crate) checkpoint: CheckpointRecord,
    pub(crate) entry: EntryRecord,
    pub(crate) admission: Option<InputAdmission>,
}
""",
)
replace(
    store,
    """    BeginOperation {
        session_id: SessionId,
        lane_name: String,
        operation_id: OperationId,
        root_inbox: InboxRecord,
        checkpoint: CheckpointRecord,
        entry: EntryRecord,
        admission: Option<InputAdmission>,
        reply: oneshot::Sender<Result<BeginOperationResult, StoreError>>,
    },""",
    """    BeginOperation {
        request: BeginOperationRequest,
        reply: oneshot::Sender<Result<BeginOperationResult, StoreError>>,
    },""",
)
replace(
    store,
    """    pub(crate) async fn begin_operation(
        &self,
        session_id: SessionId,
        lane_name: impl Into<String>,
        operation_id: OperationId,
        root_inbox: InboxRecord,
        checkpoint: CheckpointRecord,
        entry: EntryRecord,
        admission: Option<InputAdmission>,
    ) -> Result<BeginOperationResult, StoreError> {
        let lane_name = lane_name.into();
        self.request(|reply| StoreCommand::BeginOperation {
            session_id,
            lane_name,
            operation_id,
            root_inbox,
            checkpoint,
            entry,
            admission,
            reply,
        })
        .await
    }""",
    """    pub(crate) async fn begin_operation(
        &self,
        request: BeginOperationRequest,
    ) -> Result<BeginOperationResult, StoreError> {
        self.request(|reply| StoreCommand::BeginOperation { request, reply })
            .await
    }""",
)

sql = "crates/ion-core/src/store/sql.rs"
replace(
    sql,
    """        StoreCommand::BeginOperation {
            session_id,
            lane_name,
            operation_id,
            root_inbox,
            checkpoint,
            entry,
            admission,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                begin_operation(
                    connection,
                    session_id,
                    &lane_name,
                    operation_id,
                    &root_inbox,
                    &checkpoint,
                    &entry,
                    admission.as_ref(),
                )
            }));
        }""",
    """        StoreCommand::BeginOperation { request, reply } => {
            let _ = reply.send(
                check_injected(fail_next_write)
                    .and_then(|()| begin_operation(connection, &request)),
            );
        }""",
)
replace(
    sql,
    """fn begin_operation(
    connection: &mut Connection,
    session_id: SessionId,
    lane_name: &str,
    operation_id: OperationId,
    root_inbox: &InboxRecord,
    checkpoint: &CheckpointRecord,
    entry: &EntryRecord,
    admission: Option<&InputAdmission>,
) -> Result<BeginOperationResult, StoreError> {
    let tx = connection.transaction()?;""",
    """fn begin_operation(
    connection: &mut Connection,
    request: &BeginOperationRequest,
) -> Result<BeginOperationResult, StoreError> {
    let session_id = request.session_id;
    let lane_name = request.lane_name.as_str();
    let operation_id = request.operation_id;
    let root_inbox = &request.root_inbox;
    let checkpoint = &request.checkpoint;
    let entry = &request.entry;
    let admission = request.admission.as_ref();
    let tx = connection.transaction()?;""",
)
replace(
    sql,
    """        begin_operation(
            &mut connection,
            session_id,
            "worker",
            operation_id,
            &root_inbox,
            &checkpoint,
            &entry,
        )""",
    """        begin_operation(
            &mut connection,
            &BeginOperationRequest {
                session_id,
                lane_name: "worker".to_owned(),
                operation_id,
                root_inbox,
                checkpoint,
                entry,
                admission: None,
            },
        )""",
)
replace(
    sql,
    """            begin_operation(
                &mut connection,
                session_id,
                "missing",
                operation_id,
                &root_inbox,
                &checkpoint,
                &entry,
            )""",
    """            begin_operation(
                &mut connection,
                &BeginOperationRequest {
                    session_id,
                    lane_name: "missing".to_owned(),
                    operation_id,
                    root_inbox,
                    checkpoint,
                    entry,
                    admission: None,
                },
            )""",
)

runtime = "crates/ion-core/src/runtime/mod.rs"
replace(
    runtime,
    """    AssistantFrame, BeginOperationResult, CheckpointPayload, CheckpointRecord, CommitRequest,
    EffectRecord, EntryRecord, InboxRecord, InboxStatus, LoadedSession, SessionRecord, SessionStore,
    SettledEffect, StoreError, ToolProgressCheckpoint, UsageRecord,
};""",
    """    AssistantFrame, BeginOperationRequest, BeginOperationResult, CheckpointPayload,
    CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord, InboxStatus,
    LoadedSession, SessionRecord, SessionStore, SettledEffect, StoreError, ToolProgressCheckpoint,
    UsageRecord,
};""",
)
replace(runtime, "        active: ActiveOperation,", "        active: Box<ActiveOperation>,")
replace(
    runtime,
    """            .begin_operation(
                self.session_id,
                lane_name,
                operation_id,
                root_inbox,
                checkpoint,
                entry.clone(),
                admission,
            )""",
    """            .begin_operation(BeginOperationRequest {
                session_id: self.session_id,
                lane_name: lane_name.to_owned(),
                operation_id,
                root_inbox,
                checkpoint,
                entry: entry.clone(),
                admission,
            })""",
)
replace(
    runtime,
    "            active: ActiveOperation {",
    "            active: Box::new(ActiveOperation {",
)
replace(
    runtime,
    """                pending_inputs: Vec::new(),
            },
            entry_id,""",
    """                pending_inputs: Vec::new(),
            }),
            entry_id,""",
)
replace(
    runtime,
    "                self.start_active(&lane_name, active);",
    "                self.start_active(&lane_name, *active);",
    count=2,
)
replace(
    runtime,
    "                } => (active, entry_id),",
    "                } => (*active, entry_id),",
)
replace(
    runtime,
    "                self.start_active(lane_name, active);",
    "                self.start_active(lane_name, *active);",
)

reconcile = "crates/ion-core/src/tests/reconcile.rs"
replace(
    reconcile,
    """use crate::store::{
    CheckpointPayload, CheckpointRecord, EffectKind, EffectRecord, EntryRecord, InboxRecord,
    InboxStatus, OpenEffect, SessionStore,
};""",
    """use crate::store::{
    BeginOperationRequest, CheckpointPayload, CheckpointRecord, EffectKind, EffectRecord,
    EntryRecord, InboxRecord, InboxStatus, OpenEffect, SessionStore,
};""",
)
replace(
    reconcile,
    """        .begin_operation(
            session_id,
            crate::session::lane::MAIN,
            operation_id,
            root_inbox,
            checkpoint,
            entry,
            None,
        )""",
    """        .begin_operation(BeginOperationRequest {
            session_id,
            lane_name: crate::session::lane::MAIN.to_owned(),
            operation_id,
            root_inbox,
            checkpoint,
            entry,
            admission: None,
        })""",
)
