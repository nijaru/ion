from pathlib import Path
import re


def replace_once(path: str, old: str, new: str, label: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    p.write_text(text.replace(old, new))


def regex_once(path: str, pattern: str, replacement: str, label: str) -> None:
    p = Path(path)
    text = p.read_text()
    new, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    p.write_text(new)


replace_once(
    "crates/ion-core/src/store/schema.rs",
    "const SCHEMA_VERSION: i64 = 17;",
    "const SCHEMA_VERSION: i64 = 18;",
    "schema version",
)
regex_once(
    "crates/ion-core/src/store/schema.rs",
    r'''-- Current runtime accepts on the hidden main lane\..*?CREATE TRIGGER IF NOT EXISTS operation_origin_on_insert\nAFTER INSERT ON operations\nBEGIN\n.*?\nEND;\n\n''',
    '''-- Operation origins are inserted explicitly by the lane-addressable
-- acceptance transaction. This keeps lane/source-leaf ownership in the
-- store API instead of hiding it in a main-lane trigger.

''',
    "remove hard-coded operation-origin trigger",
)

replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    BeginOperation {
        session_id: SessionId,
        operation_id: OperationId,
        root_inbox: InboxRecord,
''',
    '''    BeginOperation {
        session_id: SessionId,
        lane_name: String,
        operation_id: OperationId,
        root_inbox: InboxRecord,
''',
    "store begin command lane",
)
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    /// Durably accept an operation: the operation row, its root inbox
    /// item, the initial total state, and the user entry commit as one
    /// transaction before the caller is acknowledged (DESIGN.md §9.1).
    pub async fn begin_operation(
        &self,
        session_id: SessionId,
        operation_id: OperationId,
        root_inbox: InboxRecord,
        checkpoint: CheckpointRecord,
        entry: EntryRecord,
    ) -> Result<(), StoreError> {
        self.request(|reply| StoreCommand::BeginOperation {
            session_id,
            operation_id,
            root_inbox,
            checkpoint,
            entry,
            reply,
        })
        .await
    }
''',
    '''    /// Durably accept an operation on an explicit lane: immutable
    /// origin, root inbox, initial total state, and user entry commit as one
    /// transaction before the caller is acknowledged.
    pub(crate) async fn begin_operation_on_lane(
        &self,
        session_id: SessionId,
        lane_name: &str,
        operation_id: OperationId,
        root_inbox: InboxRecord,
        checkpoint: CheckpointRecord,
        entry: EntryRecord,
    ) -> Result<(), StoreError> {
        let lane_name = lane_name.to_owned();
        self.request(|reply| StoreCommand::BeginOperation {
            session_id,
            lane_name,
            operation_id,
            root_inbox,
            checkpoint,
            entry,
            reply,
        })
        .await
    }

    /// Main-lane convenience retained while external callers are not yet
    /// lane-addressable. Runtime acceptance uses `begin_operation_on_lane`.
    pub async fn begin_operation(
        &self,
        session_id: SessionId,
        operation_id: OperationId,
        root_inbox: InboxRecord,
        checkpoint: CheckpointRecord,
        entry: EntryRecord,
    ) -> Result<(), StoreError> {
        self.begin_operation_on_lane(
            session_id,
            crate::session::lane::MAIN,
            operation_id,
            root_inbox,
            checkpoint,
            entry,
        )
        .await
    }
''',
    "store lane-addressable begin method",
)

replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        StoreCommand::BeginOperation {
            session_id,
            operation_id,
            root_inbox,
            checkpoint,
            entry,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                begin_operation(
                    connection,
                    session_id,
                    operation_id,
                    &root_inbox,
                    &checkpoint,
                    &entry,
                )
''',
    '''        StoreCommand::BeginOperation {
            session_id,
            lane_name,
            operation_id,
            root_inbox,
            checkpoint,
            entry,
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
                )
''',
    "store sql dispatch lane",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    "    insert_entry(&tx, session_id, entry)?;",
    "    insert_entry_on_lane(&tx, session_id, crate::session::lane::MAIN, entry)?;",
    "append entry main lane",
)

regex_once(
    "crates/ion-core/src/store/sql.rs",
    r'''fn begin_operation\(\n.*?\n\}\n\nfn commit\(''',
    '''fn begin_operation(
    connection: &mut Connection,
    session_id: SessionId,
    lane_name: &str,
    operation_id: OperationId,
    root_inbox: &InboxRecord,
    checkpoint: &CheckpointRecord,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let session = session_id.as_uuid().to_string();
    let operation = operation_id.as_uuid().to_string();
    let entry_id = entry.id.as_uuid().to_string();
    let (source_leaf, current, pending_entry, pending_prompt): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = tx.query_row(
        "SELECT leaf_id, current_operation_id, pending_entry_id, pending_prompt
         FROM lanes WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![&session, lane_name],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    if current.is_some() {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "lane {lane_name} already has a current operation"
        )));
    }
    let user_prompt = match &entry.entry {
        SessionEntry::UserMessage { text } => text,
        _ => {
            return Err(rusqlite::Error::InvalidParameterName(
                "operation acceptance must append its user prompt".to_owned(),
            ));
        }
    };
    if user_prompt != &checkpoint.payload.prompt || root_inbox.text.as_str() != user_prompt {
        return Err(rusqlite::Error::InvalidParameterName(
            "operation prompt disagrees with its accepted entry".to_owned(),
        ));
    }
    match (pending_entry, pending_prompt) {
        (None, None) => {}
        (Some(reserved_entry), Some(reserved_prompt))
            if reserved_entry == entry_id && reserved_prompt.as_str() == user_prompt => {}
        _ => {
            return Err(rusqlite::Error::InvalidParameterName(
                "operation does not match the lane's reserved next run".to_owned(),
            ));
        }
    }

    let accepted_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(accepted_seq), 0) + 1 FROM operations WHERE session_id = ?1",
        [&session],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
         VALUES (?1, ?2, 'run', ?3, ?4)",
        rusqlite::params![&operation, &session, now_ms(), accepted_seq],
    )?;
    tx.execute(
        "INSERT INTO operation_origins (operation_id, session_id, lane_name, source_leaf_id)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![&operation, &session, lane_name, source_leaf],
    )?;
    insert_inbox(&tx, session_id, operation_id, root_inbox)?;
    insert_capability_snapshot(&tx, &checkpoint.capability_snapshot)?;
    insert_checkpoint(&tx, operation_id, checkpoint)?;
    insert_entry_on_lane(&tx, session_id, lane_name, entry)?;
    let changed = tx.execute(
        "UPDATE lanes SET
            current_operation_id = ?3,
            pending_entry_id = NULL,
            pending_prompt = NULL,
            updated_at = ?4
         WHERE session_id = ?1 AND name = ?2 AND current_operation_id IS NULL",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            lane_name,
            operation_id.as_uuid().to_string(),
            now_ms(),
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "lane {lane_name} lost operation admission"
        )));
    }
    tx.commit()?;
    Ok(())
}

fn commit(''',
    "lane-addressable begin operation",
)

replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''fn commit(connection: &mut Connection, request: &CommitRequest) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let operation_id = request.operation_id.as_uuid().to_string();
    let current: Option<String> = tx.query_row(
        "SELECT current_operation_id FROM lanes WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![
            request.session_id.as_uuid().to_string(),
            crate::session::lane::MAIN,
        ],
        |row| row.get(0),
    )?;
    if current.as_deref() != Some(operation_id.as_str()) {
        return Err(rusqlite::Error::InvalidParameterName(
            "operation is not the main lane's current operation".to_owned(),
        ));
    }
''',
    '''fn commit(connection: &mut Connection, request: &CommitRequest) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let operation_id = request.operation_id.as_uuid().to_string();
    let session_id = request.session_id.as_uuid().to_string();
    let lane_name: String = match tx.query_row(
        "SELECT origin.lane_name
         FROM operation_origins origin
         JOIN lanes lane
           ON lane.session_id = origin.session_id AND lane.name = origin.lane_name
         WHERE origin.operation_id = ?1 AND origin.session_id = ?2
           AND lane.current_operation_id = ?1",
        rusqlite::params![&operation_id, &session_id],
        |row| row.get(0),
    ) {
        Ok(lane_name) => lane_name,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(rusqlite::Error::InvalidParameterName(
                "operation is not current on its immutable origin lane".to_owned(),
            ));
        }
        Err(err) => return Err(err),
    };
''',
    "commit origin lane guard",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    "        insert_entry(&tx, request.session_id, entry)?;",
    "        insert_entry_on_lane(&tx, request.session_id, &lane_name, entry)?;",
    "commit entry origin lane",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        let changed = tx.execute(
            "UPDATE lanes SET current_operation_id = NULL, updated_at = ?4
             WHERE session_id = ?1 AND name = ?2 AND current_operation_id = ?3",
            rusqlite::params![
                request.session_id.as_uuid().to_string(),
                crate::session::lane::MAIN,
                operation_id,
                now_ms(),
            ],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidParameterName(
                "terminal operation no longer owns the main lane".to_owned(),
            ));
        }
''',
    '''        let changed = tx.execute(
            "UPDATE lanes SET current_operation_id = NULL, updated_at = ?4
             WHERE session_id = ?1 AND name = ?2 AND current_operation_id = ?3",
            rusqlite::params![
                request.session_id.as_uuid().to_string(),
                &lane_name,
                &operation_id,
                now_ms(),
            ],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "terminal operation no longer owns lane {lane_name}"
            )));
        }
''',
    "terminal origin lane release",
)

regex_once(
    "crates/ion-core/src/store/sql.rs",
    r'''fn insert_entry\(\n.*?\n\}\n\nfn load_main_lane\(''',
    '''fn insert_entry_on_lane(
    connection: &Connection,
    session_id: SessionId,
    lane_name: &str,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
    let expected_seq: i64 = connection.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM entries WHERE session_id = ?1",
        [session_id.as_uuid().to_string()],
        |row| row.get(0),
    )?;
    if entry.seq != expected_seq as u64 {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "entry sequence {} is not the next durable sequence {expected_seq}",
            entry.seq
        )));
    }

    let (leaf_id, config): (Option<String>, String) = connection.query_row(
        "SELECT leaf_id, config FROM lanes WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![session_id.as_uuid().to_string(), lane_name],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let expected_parent = leaf_id
        .as_deref()
        .map(|raw| {
            crate::ids::EntryId::parse(raw).ok_or_else(|| {
                rusqlite::Error::InvalidParameterName(format!(
                    "lane {lane_name} has invalid leaf id"
                ))
            })
        })
        .transpose()?;
    if entry.parent != expected_parent {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "entry parent does not match lane {lane_name} leaf"
        )));
    }
    let node = crate::session::tree::Entry {
        id: entry.id,
        parent: entry.parent,
        seq: entry.seq,
        value: entry.entry.clone(),
    };
    let payload = serde_json::to_string(&node.value)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    connection.execute(
        "INSERT INTO entries (session_id, seq, id, parent_id, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            node.seq as i64,
            node.id.as_uuid().to_string(),
            node.parent.map(|parent| parent.as_uuid().to_string()),
            entry_kind(&node.value),
            payload,
            now_ms(),
        ],
    )?;

    let mut config: crate::session::lane::Config = serde_json::from_str(&config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    if let SessionEntry::ModelChanged { model_ref } = &node.value {
        config.model_ref.clone_from(model_ref);
    }
    let config = serde_json::to_string(&config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    let changed = connection.execute(
        "UPDATE lanes SET leaf_id = ?3, config = ?4, updated_at = ?5
         WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            lane_name,
            node.id.as_uuid().to_string(),
            config,
            now_ms(),
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "lane {lane_name} is missing"
        )));
    }
    Ok(())
}

fn load_main_lane(''',
    "lane-addressable entry insertion",
)

replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    '''        self.store
            .begin_operation(
                self.session_id,
                operation_id,
''',
    '''        self.store
            .begin_operation_on_lane(
                self.session_id,
                crate::session::lane::MAIN,
                operation_id,
''',
    "runtime explicit acceptance lane",
)
