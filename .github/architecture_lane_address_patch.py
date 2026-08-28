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


# Schema: operation origin is now written explicitly by the admission transaction.
replace_once(
    "crates/ion-core/src/store/schema.rs",
    "const SCHEMA_VERSION: i64 = 17;",
    "const SCHEMA_VERSION: i64 = 18;",
    "schema version",
)
regex_once(
    "crates/ion-core/src/store/schema.rs",
    r'''-- Current runtime accepts on the hidden main lane\..*?CREATE TRIGGER IF NOT EXISTS operation_origin_on_insert\nAFTER INSERT ON operations\nBEGIN\n.*?\nEND;\n\n''',
    '''-- Operation origin is inserted explicitly by the same store transaction that
-- admits an operation. SQLite foreign keys validate the supplied lane and source
-- leaf; there is no trigger that guesses either fact from a hidden default lane.
''',
    "remove inferred origin trigger",
)
replace_once(
    "crates/ion-core/src/store/schema.rs",
    '''        connection
            .execute(
                "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
                 VALUES ('operation', 'session', 'run', 0, 1)",
                [],
            )
            .expect("operation");
        connection
''',
    '''        connection
            .execute(
                "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
                 VALUES ('operation', 'session', 'run', 0, 1)",
                [],
            )
            .expect("operation");
        connection
            .execute(
                "INSERT INTO operation_origins
                    (operation_id, session_id, lane_name, source_leaf_id)
                 VALUES ('operation', 'session', 'main', NULL)",
                [],
            )
            .expect("origin");
        connection
''',
    "seed explicit origin",
)
replace_once(
    "crates/ion-core/src/store/schema.rs",
    '''        connection
            .execute(
                "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
                 VALUES ('operation-2', 'session', 'run', 2, 2)",
                [],
            )
            .expect("second operation");
        let source: Option<String> = connection
''',
    '''        connection
            .execute(
                "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
                 VALUES ('operation-2', 'session', 'run', 2, 2)",
                [],
            )
            .expect("second operation");
        connection
            .execute(
                "INSERT INTO operation_origins
                    (operation_id, session_id, lane_name, source_leaf_id)
                 VALUES ('operation-2', 'session', 'main', 'entry-1')",
                [],
            )
            .expect("second origin");
        let source: Option<String> = connection
''',
    "non-root explicit origin",
)
regex_once(
    "crates/ion-core/src/store/schema.rs",
    r'''    #\[test\]\n    fn operation_acceptance_requires_main_lane\(\) \{.*?\n    \}\n\}''',
    '''    #[test]
    fn operation_origin_requires_an_existing_lane() {
        let mut connection = Connection::open_in_memory().expect("database");
        create_fresh(&mut connection).expect("schema");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        connection
            .execute(
                "INSERT INTO sessions (id, created_at, updated_at, cwd, title, parent_session_id)
                 VALUES ('orphan', 0, 0, '/tmp', '', NULL)",
                [],
            )
            .expect("session");
        connection
            .execute(
                "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
                 VALUES ('orphan-operation', 'orphan', 'run', 0, 1)",
                [],
            )
            .expect("raw operation row");
        assert!(
            connection
                .execute(
                    "INSERT INTO operation_origins
                        (operation_id, session_id, lane_name, source_leaf_id)
                     VALUES ('orphan-operation', 'orphan', 'missing', NULL)",
                    [],
                )
                .is_err(),
            "an immutable origin cannot name a lane that does not exist"
        );
    }
}''',
    "origin lane FK test",
)

# Store command/API boundary: lane is an explicit admission/config/input address.
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    BeginOperation {
        session_id: SessionId,
        operation_id: OperationId,
''',
    '''    BeginOperation {
        session_id: SessionId,
        lane_name: String,
        operation_id: OperationId,
''',
    "begin command lane",
)
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    AppendEntry {
        session_id: SessionId,
        entry: EntryRecord,
''',
    '''    AppendEntry {
        session_id: SessionId,
        lane_name: String,
        entry: EntryRecord,
''',
    "append command lane",
)
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    SetMainLaneConfig {
        session_id: SessionId,
        config: crate::session::lane::Config,
''',
    '''    SetLaneConfig {
        session_id: SessionId,
        lane_name: String,
        config: crate::session::lane::Config,
''',
    "config command lane",
)
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    QueueMainNextRun {
        session_id: SessionId,
        next_run: crate::session::lane::NextRun,
''',
    '''    QueueNextRun {
        session_id: SessionId,
        lane_name: String,
        next_run: crate::session::lane::NextRun,
''',
    "next-run command lane",
)
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    pub async fn begin_operation(
        &self,
        session_id: SessionId,
        operation_id: OperationId,
''',
    '''    pub async fn begin_operation(
        &self,
        session_id: SessionId,
        lane_name: impl Into<String>,
        operation_id: OperationId,
''',
    "begin API signature",
)
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    ) -> Result<(), StoreError> {
        self.request(|reply| StoreCommand::BeginOperation {
            session_id,
            operation_id,
''',
    '''    ) -> Result<(), StoreError> {
        let lane_name = lane_name.into();
        self.request(|reply| StoreCommand::BeginOperation {
            session_id,
            lane_name,
            operation_id,
''',
    "begin API command",
)
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    pub async fn append_entry(
        &self,
        session_id: SessionId,
        entry: EntryRecord,
    ) -> Result<(), StoreError> {
        self.request(|reply| StoreCommand::AppendEntry {
            session_id,
            entry,
''',
    '''    pub async fn append_entry(
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
''',
    "append API lane",
)
regex_once(
    "crates/ion-core/src/store/mod.rs",
    r'''    /// Replace the total configuration for future work on the hidden `main`\n    /// lane\. This stays crate-private until callers can address arbitrary\n    /// lanes directly\.\n    pub\(crate\) async fn set_main_lane_config\(\n        &self,\n        session_id: SessionId,\n        config: crate::session::lane::Config,\n    \) -> Result<\(\), StoreError> \{\n        self\.request\(\|reply\| StoreCommand::SetMainLaneConfig \{\n            session_id,\n            config,\n            reply,\n        \}\)\n        \.await\n    \}\n\n    /// Reserve the next run on the hidden `main` lane without\n    /// creating an operation or semantic entry yet\.\n    pub\(crate\) async fn queue_main_next_run\(\n        &self,\n        session_id: SessionId,\n        next_run: crate::session::lane::NextRun,\n    \) -> Result<\(\), StoreError> \{\n        self\.request\(\|reply\| StoreCommand::QueueMainNextRun \{\n            session_id,\n            next_run,\n            reply,\n        \}\)\n        \.await\n    \}''',
    '''    /// Replace the total configuration for future work on one durable lane.
    pub(crate) async fn set_lane_config(
        &self,
        session_id: SessionId,
        lane_name: impl Into<String>,
        config: crate::session::lane::Config,
    ) -> Result<(), StoreError> {
        let lane_name = lane_name.into();
        self.request(|reply| StoreCommand::SetLaneConfig {
            session_id,
            lane_name,
            config,
            reply,
        })
        .await
    }

    /// Reserve the next run on one durable lane without creating an operation
    /// or semantic entry yet.
    pub(crate) async fn queue_next_run(
        &self,
        session_id: SessionId,
        lane_name: impl Into<String>,
        next_run: crate::session::lane::NextRun,
    ) -> Result<(), StoreError> {
        let lane_name = lane_name.into();
        self.request(|reply| StoreCommand::QueueNextRun {
            session_id,
            lane_name,
            next_run,
            reply,
        })
        .await
    }''',
    "lane-addressable config and queue APIs",
)

# Store SQL dispatch + lane-addressable transactions.
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        StoreCommand::BeginOperation {
            session_id,
            operation_id,
''',
    '''        StoreCommand::BeginOperation {
            session_id,
            lane_name,
            operation_id,
''',
    "sql begin dispatch destructure",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''                    connection,
                    session_id,
                    operation_id,
''',
    '''                    connection,
                    session_id,
                    &lane_name,
                    operation_id,
''',
    "sql begin dispatch lane",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        StoreCommand::AppendEntry {
            session_id,
            entry,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                append_entry(connection, session_id, &entry).map_err(StoreError::from)
            }));
        }
''',
    '''        StoreCommand::AppendEntry {
            session_id,
            lane_name,
            entry,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                append_entry(connection, session_id, &lane_name, &entry).map_err(StoreError::from)
            }));
        }
''',
    "sql append dispatch lane",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        StoreCommand::SetMainLaneConfig {
            session_id,
            config,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                set_main_lane_config(connection, session_id, &config).map_err(StoreError::from)
            }));
        }
        StoreCommand::QueueMainNextRun {
            session_id,
            next_run,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                queue_main_next_run(connection, session_id, &next_run).map_err(StoreError::from)
            }));
        }
''',
    '''        StoreCommand::SetLaneConfig {
            session_id,
            lane_name,
            config,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                set_lane_config(connection, session_id, &lane_name, &config).map_err(StoreError::from)
            }));
        }
        StoreCommand::QueueNextRun {
            session_id,
            lane_name,
            next_run,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                queue_next_run(connection, session_id, &lane_name, &next_run).map_err(StoreError::from)
            }));
        }
''',
    "sql config queue dispatch",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''fn set_main_lane_config(
    connection: &mut Connection,
    session_id: SessionId,
    config: &crate::session::lane::Config,
''',
    '''fn set_lane_config(
    connection: &mut Connection,
    session_id: SessionId,
    lane_name: &str,
    config: &crate::session::lane::Config,
''',
    "set lane config signature",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''            session_id.as_uuid().to_string(),
            crate::session::lane::MAIN,
            config,
''',
    '''            session_id.as_uuid().to_string(),
            lane_name,
            config,
''',
    "set lane config params",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''            "main lane is missing".to_owned(),
        ));
    }
    tx.execute(
''',
    '''            format!("lane {lane_name:?} is missing"),
        ));
    }
    tx.execute(
''',
    "set lane config error",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''fn queue_main_next_run(
    connection: &mut Connection,
    session_id: SessionId,
    next_run: &crate::session::lane::NextRun,
''',
    '''fn queue_next_run(
    connection: &mut Connection,
    session_id: SessionId,
    lane_name: &str,
    next_run: &crate::session::lane::NextRun,
''',
    "queue lane signature",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''            session_id.as_uuid().to_string(),
            crate::session::lane::MAIN,
            entry_id,
            next_run.prompt,
''',
    '''            session_id.as_uuid().to_string(),
            lane_name,
            entry_id,
            next_run.prompt,
''',
    "queue lane params",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''            "main lane cannot reserve another next run".to_owned(),
''',
    '''            format!("lane {lane_name:?} cannot reserve another next run"),
''',
    "queue lane error",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''fn append_entry(
    connection: &mut Connection,
    session_id: SessionId,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    insert_entry(&tx, session_id, entry)?;
''',
    '''fn append_entry(
    connection: &mut Connection,
    session_id: SessionId,
    lane_name: &str,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    insert_entry(&tx, session_id, lane_name, entry)?;
''',
    "append lane signature",
)
regex_once(
    "crates/ion-core/src/store/sql.rs",
    r'''fn begin_operation\(\n    connection: &mut Connection,\n    session_id: SessionId,\n    operation_id: OperationId,\n    root_inbox: &InboxRecord,\n    checkpoint: &CheckpointRecord,\n    entry: &EntryRecord,\n\) -> Result<\(\), rusqlite::Error> \{.*?\n\}\n\nfn commit\(''',
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
        rusqlite::params![session, lane_name],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    if current.is_some() {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "lane {lane_name:?} already has a current operation"
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
        rusqlite::params![operation, session, now_ms(), accepted_seq],
    )?;
    tx.execute(
        "INSERT INTO operation_origins
            (operation_id, session_id, lane_name, source_leaf_id)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![operation, session, lane_name, source_leaf],
    )?;
    insert_inbox(&tx, session_id, operation_id, root_inbox)?;
    insert_capability_snapshot(&tx, &checkpoint.capability_snapshot)?;
    insert_checkpoint(&tx, operation_id, checkpoint)?;
    insert_entry(&tx, session_id, lane_name, entry)?;
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
            "lane {lane_name:?} lost operation admission"
        )));
    }
    tx.commit()?;
    Ok(())
}

fn commit(''',
    "lane-addressable begin transaction",
)
regex_once(
    "crates/ion-core/src/store/sql.rs",
    r'''    let operation_id = request\.operation_id\.as_uuid\(\)\.to_string\(\);\n    let current: Option<String> = tx\.query_row\(\n        "SELECT current_operation_id FROM lanes WHERE session_id = \?1 AND name = \?2",\n        rusqlite::params!\[\n            request\.session_id\.as_uuid\(\)\.to_string\(\),\n            crate::session::lane::MAIN,\n        \],\n        \|row\| row\.get\(0\),\n    \)\?;\n    if current\.as_deref\(\) != Some\(operation_id\.as_str\(\)\) \{\n        return Err\(rusqlite::Error::InvalidParameterName\(\n            "operation is not the main lane's current operation"\.to_owned\(\),\n        \)\);\n    \}''',
    '''    let operation_id = request.operation_id.as_uuid().to_string();
    let (lane_name, current): (String, Option<String>) = tx.query_row(
        "SELECT origin.lane_name, lane.current_operation_id
         FROM operation_origins origin
         JOIN lanes lane
           ON lane.session_id = origin.session_id AND lane.name = origin.lane_name
         WHERE origin.operation_id = ?1 AND origin.session_id = ?2",
        rusqlite::params![operation_id, request.session_id.as_uuid().to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if current.as_deref() != Some(operation_id.as_str()) {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "operation is not lane {lane_name:?}'s current operation"
        )));
    }''',
    "commit derives lane from origin",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        insert_entry(&tx, request.session_id, entry)?;
''',
    '''        insert_entry(&tx, request.session_id, &lane_name, entry)?;
''',
    "commit entry lane",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''                request.session_id.as_uuid().to_string(),
                crate::session::lane::MAIN,
                operation_id,
                now_ms(),
''',
    '''                request.session_id.as_uuid().to_string(),
                lane_name,
                operation_id,
                now_ms(),
''',
    "terminal lane params",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''                "terminal operation no longer owns the main lane".to_owned(),
''',
    '''                "terminal operation no longer owns its immutable origin lane".to_owned(),
''',
    "terminal lane error",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''fn insert_entry(
    connection: &Connection,
    session_id: SessionId,
    entry: &EntryRecord,
''',
    '''fn insert_entry(
    connection: &Connection,
    session_id: SessionId,
    lane_name: &str,
    entry: &EntryRecord,
''',
    "insert entry lane signature",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        rusqlite::params![session_id.as_uuid().to_string(), crate::session::lane::MAIN],
''',
    '''        rusqlite::params![session_id.as_uuid().to_string(), lane_name],
''',
    "insert entry lane query",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''                rusqlite::Error::InvalidParameterName("main lane has invalid leaf id".to_owned())
''',
    '''                rusqlite::Error::InvalidParameterName(format!("lane {lane_name:?} has invalid leaf id"))
''',
    "insert entry leaf error",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''            "entry parent does not match the main lane leaf".to_owned(),
''',
    '''            format!("entry parent does not match lane {lane_name:?}'s leaf"),
''',
    "insert entry parent error",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''            session_id.as_uuid().to_string(),
            crate::session::lane::MAIN,
            node.id.as_uuid().to_string(),
            config,
''',
    '''            session_id.as_uuid().to_string(),
            lane_name,
            node.id.as_uuid().to_string(),
            config,
''',
    "insert entry update lane",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''            "main lane is missing".to_owned(),
        ));
    }
    Ok(())
}

fn load_main_lane''',
    '''            format!("lane {lane_name:?} is missing"),
        ));
    }
    Ok(())
}

fn load_main_lane''',
    "insert entry missing lane error",
)

# Existing SQL unit test still exercises main, but through the generalized helpers.
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        append_entry(&mut connection, session_id, &first_record).expect("first entry");
''',
    '''        append_entry(
            &mut connection,
            session_id,
            crate::session::lane::MAIN,
            &first_record,
        )
        .expect("first entry");
''',
    "first test append lane",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        append_entry(&mut connection, session_id, &second_record).expect("second entry");
''',
    '''        append_entry(
            &mut connection,
            session_id,
            crate::session::lane::MAIN,
            &second_record,
        )
        .expect("second entry");
''',
    "second test append lane",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        set_main_lane_config(
            &mut connection,
            session_id,
            &crate::session::lane::Config::new("model-b"),
''',
    '''        set_lane_config(
            &mut connection,
            session_id,
            crate::session::lane::MAIN,
            &crate::session::lane::Config::new("model-b"),
''',
    "test config lane",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(leaf_after, leaf_before);
    }
}
''',
    '''        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(leaf_after, leaf_before);
    }

    #[test]
    fn operation_admission_uses_the_explicit_lane_and_captures_its_source_leaf() {
        let mut connection = Connection::open_in_memory().expect("database");
        crate::store::schema::create_fresh(&mut connection).expect("schema");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        let session_id = SessionId::generate();
        create_session(
            &mut connection,
            &SessionRecord {
                id: session_id,
                cwd: "/tmp".to_owned(),
                title: String::new(),
                initial_model_ref: "model-a".to_owned(),
                parent_session_id: None,
            },
        )
        .expect("session");
        let config = serde_json::to_string(&crate::session::lane::Config::new("model-b"))
            .expect("config");
        connection
            .execute(
                "INSERT INTO lanes
                    (session_id, name, leaf_id, current_operation_id,
                     pending_entry_id, pending_prompt, config, created_at, updated_at)
                 VALUES (?1, 'worker', NULL, NULL, NULL, NULL, ?2, 0, 0)",
                rusqlite::params![session_id.as_uuid().to_string(), config],
            )
            .expect("worker lane");

        let operation_id = OperationId::generate();
        let snapshot = crate::context::CapabilitySnapshot::new(Vec::new());
        let prompt = "worker prompt".to_owned();
        let root_inbox = InboxRecord {
            id: InboxId::generate(),
            kind: InboxKind::Prompt,
            text: prompt.clone(),
            status: InboxStatus::Applied,
        };
        let checkpoint = CheckpointRecord {
            state_seq: 1,
            payload: CheckpointPayload {
                state: OperationState::Accepted,
                cancel_requested: false,
                prompt: prompt.clone(),
                capability_snapshot_id: snapshot.id.clone(),
                open_effect: None,
            },
            capability_snapshot: snapshot,
        };
        let entry = EntryRecord::provision(1, SessionEntry::UserMessage { text: prompt });
        let entry_id = entry.id;
        begin_operation(
            &mut connection,
            session_id,
            "worker",
            operation_id,
            &root_inbox,
            &checkpoint,
            &entry,
        )
        .expect("worker admission");

        let (lane_name, source_leaf): (String, Option<String>) = connection
            .query_row(
                "SELECT lane_name, source_leaf_id FROM operation_origins WHERE operation_id = ?1",
                [operation_id.as_uuid().to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("origin");
        let worker_leaf: Option<String> = connection
            .query_row(
                "SELECT leaf_id FROM lanes WHERE session_id = ?1 AND name = 'worker'",
                [session_id.as_uuid().to_string()],
                |row| row.get(0),
            )
            .expect("worker leaf");
        let main_leaf: Option<String> = connection
            .query_row(
                "SELECT leaf_id FROM lanes WHERE session_id = ?1 AND name = 'main'",
                [session_id.as_uuid().to_string()],
                |row| row.get(0),
            )
            .expect("main leaf");
        assert_eq!(lane_name, "worker");
        assert!(source_leaf.is_none());
        assert_eq!(worker_leaf.as_deref(), Some(entry_id.as_uuid().to_string().as_str()));
        assert!(main_leaf.is_none());
    }

    #[test]
    fn operation_admission_to_a_missing_lane_is_atomic() {
        let mut connection = Connection::open_in_memory().expect("database");
        crate::store::schema::create_fresh(&mut connection).expect("schema");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        let session_id = SessionId::generate();
        create_session(
            &mut connection,
            &SessionRecord {
                id: session_id,
                cwd: "/tmp".to_owned(),
                title: String::new(),
                initial_model_ref: "model-a".to_owned(),
                parent_session_id: None,
            },
        )
        .expect("session");
        let operation_id = OperationId::generate();
        let snapshot = crate::context::CapabilitySnapshot::new(Vec::new());
        let prompt = "missing".to_owned();
        let root_inbox = InboxRecord {
            id: InboxId::generate(),
            kind: InboxKind::Prompt,
            text: prompt.clone(),
            status: InboxStatus::Applied,
        };
        let checkpoint = CheckpointRecord {
            state_seq: 1,
            payload: CheckpointPayload {
                state: OperationState::Accepted,
                cancel_requested: false,
                prompt: prompt.clone(),
                capability_snapshot_id: snapshot.id.clone(),
                open_effect: None,
            },
            capability_snapshot: snapshot,
        };
        let entry = EntryRecord::provision(1, SessionEntry::UserMessage { text: prompt });
        assert!(
            begin_operation(
                &mut connection,
                session_id,
                "missing",
                operation_id,
                &root_inbox,
                &checkpoint,
                &entry,
            )
            .is_err()
        );
        let operations: i64 = connection
            .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
            .expect("operation count");
        let entries: i64 = connection
            .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
            .expect("entry count");
        assert_eq!(operations, 0);
        assert_eq!(entries, 0);
    }
}
''',
    "lane-addressable SQL tests",
)

# Runtime owns main today but now addresses it explicitly at the store boundary.
replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    '''            .queue_main_next_run(self.session_id, next_run.clone())
''',
    '''            .queue_next_run(
                self.session_id,
                crate::session::lane::MAIN,
                next_run.clone(),
            )
''',
    "runtime queue lane",
)
replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    '''            .begin_operation(
                self.session_id,
                operation_id,
''',
    '''            .begin_operation(
                self.session_id,
                crate::session::lane::MAIN,
                operation_id,
''',
    "runtime begin lane",
)
replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    '''            .set_main_lane_config(
                self.session_id,
                crate::session::lane::Config::new(model_ref.clone()),
''',
    '''            .set_lane_config(
                self.session_id,
                crate::session::lane::MAIN,
                crate::session::lane::Config::new(model_ref.clone()),
''',
    "runtime config lane",
)

# Architecture checkpoint: total lane state is complete; explicit admission is this slice.
replace_once(
    "docs/architecture-v2.md",
    '''The busy-lane queueing migration is now the active checkpoint: `pending_next_run` owns a provisioned semantic `EntryId`, while `OperationId` is provisioned only when the lane actually accepts the run. SQLite persists the pending identity without inventing it, and no queued `Accepted` operation exists.
''',
    '''Total lane state and busy-lane queueing are now complete: `pending_next_run` owns a provisioned semantic `EntryId`, while `OperationId` is provisioned only when the lane actually accepts the run. SQLite persists the pending identity without inventing it, and no queued `Accepted` operation exists.

The active checkpoint is lane-addressable operation admission. The store receives the accepting lane explicitly, captures that lane's exact source leaf into immutable `operation_origins` in the same transaction, and later commits derive lane ownership from that immutable origin rather than trusting the runtime to repeat it.
''',
    "architecture checkpoint",
)
replace_once(
    "docs/architecture-v2.md",
    '''1. Finish and validate total lane state (`leaf`, `current_operation`, `pending_next_run`) with the provision-before-persistence identity rule above.
2. Make operation acceptance explicitly lane-addressable over the durable lane/source-leaf origin contract.
3. Generalize the session owner from hidden `main` to multiple lanes while retaining one writer and concurrent slow effects.
''',
    '''1. Finish and validate lane-addressable operation acceptance over the durable lane/source-leaf origin contract.
2. Generalize the session owner from hidden `main` to multiple lanes while retaining one writer and concurrent slow effects.
''',
    "architecture next order",
)
