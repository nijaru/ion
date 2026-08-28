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
    "crates/ion-core/src/store/mod.rs",
    '''pub struct LoadedSession {
    pub session: SessionRecord,
    pub(crate) lane: crate::session::lane::Lane,
    pub entries: Vec<EntryRecord>,
    pub operations: Vec<LoadedOperation>,
    pub pending_inbox: Vec<InboxRecord>,
    pub assistant_frames: Vec<AssistantFrame>,
''',
    '''pub struct LoadedSession {
    pub session: SessionRecord,
    /// Complete durable lane state over the shared conversation tree.
    pub(crate) lanes: Vec<crate::session::lane::Lane>,
    /// Complete immutable conversation tree in global durable sequence order.
    pub entries: Vec<EntryRecord>,
    pub operations: Vec<LoadedOperation>,
    pub assistant_frames: Vec<AssistantFrame>,
''',
    "LoadedSession all lanes",
)
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    pub latest: (u64, CheckpointPayload),
    pub capability_snapshot: crate::context::CapabilitySnapshot,
}
''',
    '''    pub latest: (u64, CheckpointPayload),
    pub capability_snapshot: crate::context::CapabilitySnapshot,
    /// Pending durable input owned by this operation, never session-global.
    pub(crate) pending_inbox: Vec<InboxRecord>,
}
''',
    "LoadedOperation inbox ownership",
)

regex_once(
    "crates/ion-core/src/store/sql.rs",
    r'''fn load_main_lane\(.*?\n\}\n\nfn load\(connection: &Connection, session_id: SessionId\) -> Result<LoadedSession, StoreError> \{.*?\n    let mut statement = connection.prepare\(\n        "SELECT o\.id, o\.accepted_seq, origin\.lane_name, origin\.source_leaf_id,''',
    '''fn load_lanes(
    connection: &Connection,
    session_id: SessionId,
) -> Result<Vec<crate::session::lane::Lane>, StoreError> {
    struct RawLane {
        name: String,
        leaf: Option<String>,
        current_operation: Option<String>,
        pending_entry: Option<String>,
        pending_prompt: Option<String>,
        config: String,
    }

    let mut statement = connection.prepare(
        "SELECT name, leaf_id, current_operation_id, pending_entry_id, pending_prompt, config
         FROM lanes WHERE session_id = ?1 ORDER BY name",
    )?;
    let mut rows = statement.query([session_id.as_uuid().to_string()])?;
    let mut lanes = Vec::new();
    while let Some(row) = rows.next()? {
        let raw = RawLane {
            name: row.get(0)?,
            leaf: row.get(1)?,
            current_operation: row.get(2)?,
            pending_entry: row.get(3)?,
            pending_prompt: row.get(4)?,
            config: row.get(5)?,
        };
        let lane_name = raw.name.clone();
        let leaf = raw
            .leaf
            .as_deref()
            .map(|value| {
                crate::ids::EntryId::parse(value).ok_or_else(|| {
                    StoreError::Sqlite(format!("lane {lane_name:?} has a corrupt leaf id"))
                })
            })
            .transpose()?;
        let current_operation = raw
            .current_operation
            .as_deref()
            .map(|value| {
                Uuid::parse_str(value)
                    .map(crate::ids::OperationId::from_uuid)
                    .map_err(|_| {
                        StoreError::Sqlite(format!(
                            "lane {lane_name:?} has a corrupt current operation id"
                        ))
                    })
            })
            .transpose()?;
        let pending_next_run = match (raw.pending_entry, raw.pending_prompt) {
            (None, None) => None,
            (Some(entry), Some(prompt)) => Some(crate::session::lane::NextRun {
                entry_id: crate::ids::EntryId::parse(&entry).ok_or_else(|| {
                    StoreError::Sqlite(format!(
                        "lane {lane_name:?} has a corrupt pending entry id"
                    ))
                })?,
                prompt,
            }),
            _ => {
                return Err(StoreError::Sqlite(format!(
                    "lane {lane_name:?} has partial pending next-run state"
                )));
            }
        };
        let config = serde_json::from_str(&raw.config).map_err(|err| {
            StoreError::Sqlite(format!("lane {lane_name:?} has corrupt config: {err}"))
        })?;
        lanes.push(crate::session::lane::Lane {
            name: raw.name,
            state: crate::session::lane::State {
                leaf,
                current_operation,
                pending_next_run,
            },
            config,
        });
    }
    Ok(lanes)
}

fn load(connection: &Connection, session_id: SessionId) -> Result<LoadedSession, StoreError> {
    fn decode<T: serde::de::DeserializeOwned>(
        what: &'static str,
        raw: String,
    ) -> Result<T, StoreError> {
        serde_json::from_str(&raw)
            .map_err(|err| StoreError::Sqlite(format!("corrupt {what}: {err}")))
    }

    let id = session_id.as_uuid().to_string();
    let (cwd, title, parent_session_id): (String, String, Option<String>) = connection
        .query_row(
            "SELECT cwd, title, parent_session_id FROM sessions WHERE id = ?1",
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound(session_id),
            other => StoreError::from(other),
        })?;
    let lanes = load_lanes(connection, session_id)?;
    let main_lane = lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .ok_or_else(|| StoreError::Sqlite("session has no main lane".to_owned()))?;
    let session = SessionRecord {
        id: session_id,
        cwd,
        title,
        parent_session_id: parent_session_id.and_then(|text| SessionId::parse(&text)),
        // Compatibility projection while launch defaults still live on the
        // session record. Durable model authority is the main lane config.
        initial_model_ref: main_lane.config.model_ref.clone(),
    };

    let mut statement = connection.prepare(
        "SELECT id, parent_id, seq, payload FROM entries WHERE session_id = ?1 ORDER BY seq",
    )?;
    let mut entries = Vec::new();
    let mut rows = statement.query(rusqlite::params![id])?;
    let mut entry_ids = std::collections::HashSet::new();
    let mut expected_seq = 1_u64;
    while let Some(row) = rows.next()? {
        let stored_id: String = row.get(0)?;
        let node_id = crate::ids::EntryId::parse(&stored_id)
            .ok_or_else(|| StoreError::Sqlite("corrupt entry id".to_owned()))?;
        let parent_raw: Option<String> = row.get(1)?;
        let parent = parent_raw
            .as_deref()
            .map(|raw| {
                crate::ids::EntryId::parse(raw)
                    .ok_or_else(|| StoreError::Sqlite("corrupt entry parent id".to_owned()))
            })
            .transpose()?;
        let seq: i64 = row.get(2)?;
        let seq = u64::try_from(seq)
            .map_err(|_| StoreError::Sqlite(format!("corrupt entry seq {seq}")))?;
        if seq != expected_seq {
            return Err(StoreError::Sqlite(format!(
                "entry {node_id} has sequence {seq}, expected {expected_seq}"
            )));
        }
        if parent.is_some_and(|parent| !entry_ids.contains(&parent)) {
            return Err(StoreError::Sqlite(format!(
                "entry {node_id} points to a parent that does not precede it"
            )));
        }
        let payload: String = row.get(3)?;
        let node = crate::session::tree::Entry {
            id: node_id,
            parent,
            seq,
            value: decode("entry", payload)?,
        };
        if !entry_ids.insert(node.id) {
            return Err(StoreError::Sqlite(format!(
                "duplicate conversation entry {}",
                node.id
            )));
        }
        entries.push(EntryRecord {
            id: node.id,
            seq: node.seq,
            parent: node.parent,
            entry: node.value,
        });
        expected_seq += 1;
    }

    for lane in &lanes {
        if lane.state.leaf.is_some_and(|leaf| !entry_ids.contains(&leaf)) {
            return Err(StoreError::Sqlite(format!(
                "lane {:?} points to a missing conversation leaf",
                lane.name
            )));
        }
        if let Some(next_run) = &lane.state.pending_next_run
            && entry_ids.contains(&next_run.entry_id)
        {
            return Err(StoreError::Sqlite(format!(
                "lane {:?} pending next-run entry identity already exists",
                lane.name
            )));
        }
    }
    let lane_names: std::collections::HashSet<String> =
        lanes.iter().map(|lane| lane.name.clone()).collect();

    let mut statement = connection.prepare(
        "SELECT o.id, o.accepted_seq, origin.lane_name, origin.source_leaf_id,''',
    "all-lane tree loader prefix",
)

replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        let lane_name = lane_name.ok_or_else(|| {
            StoreError::Sqlite(format!("operation {op_id} has no immutable origin"))
        })?;
        let source_leaf = source_leaf_raw
''',
    '''        let lane_name = lane_name.ok_or_else(|| {
            StoreError::Sqlite(format!("operation {op_id} has no immutable origin"))
        })?;
        if !lane_names.contains(&lane_name) {
            return Err(StoreError::Sqlite(format!(
                "operation {op_id} names missing origin lane {lane_name:?}"
            )));
        }
        let source_leaf = source_leaf_raw
''',
    "validate operation origin lane",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''            .transpose()?;
        let checkpoint: CheckpointPayload = decode("checkpoint", payload)?;
''',
    '''            .transpose()?;
        if source_leaf.is_some_and(|leaf| !entry_ids.contains(&leaf)) {
            return Err(StoreError::Sqlite(format!(
                "operation {op_id} names a missing source leaf"
            )));
        }
        let checkpoint: CheckpointPayload = decode("checkpoint", payload)?;
''',
    "validate operation source leaf",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''            latest: (state_seq as u64, checkpoint),
            capability_snapshot,
        });
''',
    '''            latest: (state_seq as u64, checkpoint),
            capability_snapshot,
            pending_inbox: Vec::new(),
        });
''',
    "initialize operation inbox",
)
regex_once(
    "crates/ion-core/src/store/sql.rs",
    r'''    let mut open_operations = operations.*?\n    let mut statement = connection.prepare\(\n        "SELECT id, kind, text FROM inbox_items\n         WHERE session_id = \?1 AND status = 'pending' ORDER BY accepted_at",\n    \)\?;\n    let mut pending_inbox = Vec::new\(\);\n    let mut inbox_rows = statement.query\(rusqlite::params!\[id\]\)\?;\n    while let Some\(row\) = inbox_rows.next\(\)\? \{\n        let item_id: String = row.get\(0\)\?;\n        let kind: String = row.get\(1\)\?;\n        let text: String = row.get\(2\)\?;\n        let uuid = Uuid::parse_str\(&item_id\)\n            \.map_err\(\|err\| StoreError::Sqlite\(format!\("corrupt inbox id: \{err\}"\)\)\)\?;\n        pending_inbox.push\(InboxRecord \{\n            id: InboxId::from_uuid\(uuid\),\n            kind: decode\("inbox kind", kind\)\?,\n            text,\n            status: InboxStatus::Pending,\n        \}\);\n    \}\n''',
    '''    let mut open_by_lane = std::collections::HashMap::new();
    for operation in &operations {
        if matches!(&operation.latest.1.state, OperationState::Finished(_)) {
            continue;
        }
        if open_by_lane
            .insert(operation.lane_name.clone(), operation.id)
            .is_some()
        {
            return Err(StoreError::Sqlite(format!(
                "lane {:?} has multiple open operations",
                operation.lane_name
            )));
        }
    }
    for lane in &lanes {
        let open = open_by_lane.get(&lane.name).copied();
        if lane.state.current_operation != open {
            return Err(StoreError::Sqlite(format!(
                "lane {:?} current operation disagrees with durable operation state",
                lane.name
            )));
        }
    }

    let mut statement = connection.prepare(
        "SELECT operation_id, id, kind, text FROM inbox_items
         WHERE session_id = ?1 AND status = 'pending' ORDER BY accepted_at",
    )?;
    let mut inbox_rows = statement.query(rusqlite::params![id])?;
    while let Some(row) = inbox_rows.next()? {
        let operation_raw: String = row.get(0)?;
        let item_id: String = row.get(1)?;
        let kind: String = row.get(2)?;
        let text: String = row.get(3)?;
        let operation_uuid = Uuid::parse_str(&operation_raw)
            .map_err(|err| StoreError::Sqlite(format!("corrupt inbox operation id: {err}")))?;
        let operation_id = OperationId::from_uuid(operation_uuid);
        let uuid = Uuid::parse_str(&item_id)
            .map_err(|err| StoreError::Sqlite(format!("corrupt inbox id: {err}")))?;
        let operation = operations
            .iter_mut()
            .find(|operation| operation.id == operation_id)
            .ok_or_else(|| {
                StoreError::Sqlite(format!(
                    "pending inbox item {item_id} belongs to an unloaded operation"
                ))
            })?;
        operation.pending_inbox.push(InboxRecord {
            id: InboxId::from_uuid(uuid),
            kind: decode("inbox kind", kind)?,
            text,
            status: InboxStatus::Pending,
        });
    }
''',
    "per-lane open operations and per-operation inbox",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''    Ok(LoadedSession {
        session,
        lane,
        entries,
        operations,
        pending_inbox,
        assistant_frames,
''',
    '''    Ok(LoadedSession {
        session,
        lanes,
        entries,
        operations,
        assistant_frames,
''',
    "LoadedSession all lanes result",
)

replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let reopen_entry_count = loaded.as_ref().map(|loaded| loaded.entries.len());
''',
    "",
    "defer reopen branch count",
)
replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    '''            reopen_entry_count,
''',
    '''            reopen_entry_count: None,
''',
    "initialize reopen branch count",
)
replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let assistant_frames = loaded.assistant_frames;
        self.selected_model_ref = loaded.lane.config.model_ref.clone();
        self.pending_next_run = loaded.lane.state.pending_next_run.clone();
        let mut max_seq = 0;
        for record in loaded.entries {
            max_seq = max_seq.max(record.seq);
            self.entries.push(record);
        }
        self.next_entry_seq = max_seq + 1;
        for operation in loaded.operations {
''',
    '''        let assistant_frames = loaded.assistant_frames;
        let Some(main_lane) = loaded
            .lanes
            .iter()
            .find(|lane| lane.name == crate::session::lane::MAIN)
            .cloned()
        else {
            error!(session = %self.session_id, "reopened session has no main lane; fencing");
            self.closed = true;
            return;
        };
        self.selected_model_ref = main_lane.config.model_ref.clone();
        self.pending_next_run = main_lane.state.pending_next_run.clone();

        let max_seq = loaded.entries.iter().map(|record| record.seq).max().unwrap_or(0);
        let entries_by_id: std::collections::HashMap<_, _> =
            loaded.entries.iter().map(|record| (record.id, record)).collect();
        let mut branch = Vec::new();
        let mut cursor = main_lane.state.leaf;
        while let Some(entry_id) = cursor {
            let Some(record) = entries_by_id.get(&entry_id) else {
                error!(session = %self.session_id, entry = %entry_id, "main lane leaf path is incomplete; fencing");
                self.closed = true;
                return;
            };
            branch.push((*record).clone());
            cursor = record.parent;
        }
        branch.reverse();
        self.reopen_entry_count = Some(branch.len());
        self.entries = branch;
        self.next_entry_seq = max_seq + 1;

        for operation in loaded.operations {
            if operation.lane_name != crate::session::lane::MAIN {
                if !matches!(&operation.latest.1.state, OperationState::Finished(_)) {
                    error!(
                        session = %self.session_id,
                        operation = %operation.id,
                        lane = %operation.lane_name,
                        "single-lane runtime cannot host an open non-main operation; fencing"
                    );
                    self.closed = true;
                    return;
                }
                continue;
            }
''',
    "restore main branch from full tree",
)
replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    '''            let steers: Vec<InboxItem> = loaded
                .pending_inbox
                .iter()
''',
    '''            let steers: Vec<InboxItem> = operation
                .pending_inbox
                .iter()
''',
    "restore operation steers",
)
replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    '''                pending_steers: loaded
                    .pending_inbox
                    .iter()
''',
    '''                pending_steers: operation
                    .pending_inbox
                    .iter()
''',
    "restore operation steer ids",
)

replace_once(
    "crates/ion-core/src/tests/store.rs",
    '''    assert!(loaded.pending_inbox.is_empty());
''',
    '''    assert!(
        loaded
            .operations
            .iter()
            .all(|operation| operation.pending_inbox.is_empty())
    );
''',
    "restart pending inbox assertion",
)
replace_once(
    "crates/ion-core/src/tests/store.rs",
    '''    assert_eq!(loaded.pending_inbox.len(), 1);
    assert_eq!(loaded.pending_inbox[0].text, "and also check tests");
''',
    '''    let pending = &loaded.operations[0].pending_inbox;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, "and also check tests");
''',
    "steer operation inbox assertion",
)

insert_marker = '''    #[test]
    fn operation_admission_uses_the_explicit_lane_and_captures_its_source_leaf() {
'''
branched_test = '''    #[test]
    fn load_preserves_a_branched_tree_and_all_lane_cursors() {
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

        let root = EntryRecord::provision(
            1,
            SessionEntry::UserMessage {
                text: "root".to_owned(),
            },
        );
        let root_id = root.id;
        append_entry(
            &mut connection,
            session_id,
            crate::session::lane::MAIN,
            &root,
        )
        .expect("root");

        let worker_config =
            serde_json::to_string(&crate::session::lane::Config::new("model-b")).expect("config");
        connection
            .execute(
                "INSERT INTO lanes
                    (session_id, name, leaf_id, current_operation_id,
                     pending_entry_id, pending_prompt, config, created_at, updated_at)
                 VALUES (?1, 'worker', ?2, NULL, NULL, NULL, ?3, 0, 0)",
                rusqlite::params![
                    session_id.as_uuid().to_string(),
                    root_id.as_uuid().to_string(),
                    worker_config,
                ],
            )
            .expect("worker lane");

        let main_child = EntryRecord::provision(
            2,
            SessionEntry::AssistantMessage {
                text: "main".to_owned(),
            },
        )
        .after(Some(root_id));
        let main_child_id = main_child.id;
        append_entry(
            &mut connection,
            session_id,
            crate::session::lane::MAIN,
            &main_child,
        )
        .expect("main child");

        let worker_child = EntryRecord::provision(
            3,
            SessionEntry::AssistantMessage {
                text: "worker".to_owned(),
            },
        )
        .after(Some(root_id));
        let worker_child_id = worker_child.id;
        append_entry(&mut connection, session_id, "worker", &worker_child)
            .expect("worker child");

        let loaded = load(&connection, session_id).expect("branched load");
        assert_eq!(loaded.entries.len(), 3);
        assert_eq!(loaded.entries[2].parent, Some(root_id));
        let main = loaded
            .lanes
            .iter()
            .find(|lane| lane.name == crate::session::lane::MAIN)
            .expect("main lane");
        let worker = loaded
            .lanes
            .iter()
            .find(|lane| lane.name == "worker")
            .expect("worker lane");
        assert_eq!(main.state.leaf, Some(main_child_id));
        assert_eq!(worker.state.leaf, Some(worker_child_id));
        assert_eq!(worker.config.model_ref, "model-b");
    }

'''
replace_once(
    "crates/ion-core/src/store/sql.rs",
    insert_marker,
    branched_test + insert_marker,
    "branched tree load test",
)
