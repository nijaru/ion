use super::*;

pub(super) fn handle_command(
    connection: &mut Connection,
    command: StoreCommand,
    fail_next_write: &AtomicBool,
) {
    match command {
        StoreCommand::CreateSession { record, reply } => {
            let _ = reply.send(
                check_injected(fail_next_write)
                    .and_then(|()| create_session(connection, &record).map_err(StoreError::from)),
            );
        }
        StoreCommand::BeginOperation {
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
                .map_err(StoreError::from)
            }));
        }
        StoreCommand::Commit { request, reply } => {
            let _ = reply.send(
                check_injected(fail_next_write)
                    .and_then(|()| commit(connection, &request).map_err(StoreError::from)),
            );
        }
        StoreCommand::AppendEntry {
            session_id,
            entry,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                append_entry(connection, session_id, &entry).map_err(StoreError::from)
            }));
        }
        StoreCommand::SetMainLaneConfig {
            session_id,
            config,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                set_main_lane_config(connection, session_id, &config).map_err(StoreError::from)
            }));
        }
        StoreCommand::Load { session_id, reply } => {
            let _ = reply.send(load(connection, session_id));
        }
        StoreCommand::Usage { session_id, reply } => {
            let _ = reply.send(usage_rows(connection, session_id));
        }
        StoreCommand::LatestSession { reply } => {
            let _ = reply.send(latest_session(connection));
        }
        StoreCommand::Shutdown { reply } => {
            let _ = reply.send(Ok(()));
        }
        StoreCommand::UpsertAssistantFrame { frame, reply } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                upsert_assistant_frame(connection, &frame).map_err(StoreError::from)
            }));
        }
        StoreCommand::UpsertToolProgress { progress, reply } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                upsert_tool_progress(connection, &progress).map_err(StoreError::from)
            }));
        }
    }
}

fn upsert_tool_progress(
    connection: &mut Connection,
    progress: &ToolProgressCheckpoint,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO tool_progress (
            effect_id, session_id, operation_id, call_id, output, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(effect_id) DO UPDATE SET
            output = excluded.output,
            updated_at = excluded.updated_at",
        rusqlite::params![
            progress.effect_id.as_uuid().to_string(),
            progress.session_id.as_uuid().to_string(),
            progress.operation_id.as_uuid().to_string(),
            progress.call_id as i64,
            progress.output,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn upsert_assistant_frame(
    connection: &mut Connection,
    frame: &AssistantFrame,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO assistant_frames (
            effect_id, session_id, operation_id, step, frame_seq, text, thinking, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(effect_id) DO UPDATE SET
            frame_seq = excluded.frame_seq,
            text = excluded.text,
            thinking = excluded.thinking,
            updated_at = excluded.updated_at
         WHERE excluded.frame_seq > assistant_frames.frame_seq",
        rusqlite::params![
            frame.effect_id.as_uuid().to_string(),
            frame.session_id.as_uuid().to_string(),
            frame.operation_id.as_uuid().to_string(),
            frame.step as i64,
            frame.frame_seq as i64,
            frame.text,
            frame.thinking,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn latest_session(connection: &mut Connection) -> Result<Option<SessionId>, StoreError> {
    let row: Option<String> = connection
        .query_row(
            "SELECT id FROM sessions ORDER BY updated_at DESC, created_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    row.map(|id| {
        Uuid::parse_str(&id)
            .map(SessionId::from_uuid)
            .map_err(|_| StoreError::Sqlite("session id is not a uuid".into()))
    })
    .transpose()
}

fn usage_row(row: &rusqlite::Row<'_>) -> Result<UsageRow, rusqlite::Error> {
    Ok(UsageRow {
        operation_id: OperationId::from_uuid(Uuid::parse_str(&row.get::<_, String>(0)?).map_err(
            |_| rusqlite::Error::InvalidColumnType(0, "operation_id".into(), Type::Text),
        )?),
        step: row.get::<_, i64>(1)? as u64,
        input_tokens: row.get::<_, i64>(2)? as u64,
        output_tokens: row.get::<_, i64>(3)? as u64,
        cache_read_tokens: row.get::<_, i64>(4)? as u64,
        cache_write_tokens: row.get::<_, i64>(5)? as u64,
    })
}

fn usage_rows(
    connection: &mut Connection,
    session_id: SessionId,
) -> Result<Vec<UsageRow>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT operation_id, step, input_tokens, output_tokens,
                cache_read_tokens, cache_write_tokens
         FROM usage WHERE session_id = ?1 ORDER BY id",
    )?;
    let rows = statement
        .query_map([session_id.as_uuid().to_string()], usage_row)?
        .collect::<Result<Vec<_>, rusqlite::Error>>()?;
    Ok(rows)
}

fn check_injected(flag: &AtomicBool) -> Result<(), StoreError> {
    if flag.swap(false, Ordering::SeqCst) {
        Err(StoreError::Injected)
    } else {
        Ok(())
    }
}

fn create_session(
    connection: &mut Connection,
    record: &SessionRecord,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let now = now_ms();
    tx.execute(
        "INSERT INTO sessions (id, created_at, updated_at, cwd, title, parent_session_id)
         VALUES (?1, ?2, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            record.id.as_uuid().to_string(),
            now,
            record.cwd,
            record.title,
            record.parent_session_id.map(|id| id.as_uuid().to_string()),
        ],
    )?;
    let config = crate::session::lane::Config::new(record.initial_model_ref.clone());
    let config = serde_json::to_string(&config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    tx.execute(
        "INSERT INTO lanes (session_id, name, leaf_id, config, created_at, updated_at)
         VALUES (?1, ?2, NULL, ?3, ?4, ?4)",
        rusqlite::params![
            record.id.as_uuid().to_string(),
            crate::session::lane::MAIN,
            config,
            now,
        ],
    )?;
    tx.commit()
}

fn set_main_lane_config(
    connection: &mut Connection,
    session_id: SessionId,
    config: &crate::session::lane::Config,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let config = serde_json::to_string(config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    let now = now_ms();
    let changed = tx.execute(
        "UPDATE lanes SET config = ?3, updated_at = ?4
         WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            crate::session::lane::MAIN,
            config,
            now,
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(
            "main lane is missing".to_owned(),
        ));
    }
    tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![session_id.as_uuid().to_string(), now],
    )?;
    tx.commit()
}

fn append_entry(
    connection: &mut Connection,
    session_id: SessionId,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    insert_entry(&tx, session_id, entry)?;
    tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![session_id.as_uuid().to_string(), now_ms()],
    )?;
    tx.commit()
}

fn begin_operation(
    connection: &mut Connection,
    session_id: SessionId,
    operation_id: OperationId,
    root_inbox: &InboxRecord,
    checkpoint: &CheckpointRecord,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let accepted_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(accepted_seq), 0) + 1 FROM operations WHERE session_id = ?1",
        [session_id.as_uuid().to_string()],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
         VALUES (?1, ?2, 'run', ?3, ?4)",
        rusqlite::params![
            operation_id.as_uuid().to_string(),
            session_id.as_uuid().to_string(),
            now_ms(),
            accepted_seq,
        ],
    )?;
    insert_inbox(&tx, session_id, operation_id, root_inbox)?;
    insert_capability_snapshot(&tx, &checkpoint.capability_snapshot)?;
    insert_checkpoint(&tx, operation_id, checkpoint)?;
    insert_entry(&tx, session_id, entry)?;
    tx.commit()?;
    Ok(())
}

fn commit(connection: &mut Connection, request: &CommitRequest) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    insert_capability_snapshot(&tx, &request.checkpoint.capability_snapshot)?;
    for manifest in &request.context_manifests {
        insert_context_manifest(&tx, manifest)?;
    }
    insert_checkpoint(&tx, request.operation_id, &request.checkpoint)?;
    for entry in &request.entries {
        insert_entry(&tx, request.session_id, entry)?;
    }
    for effect in &request.open_effects {
        tx.execute(
            "INSERT INTO effects (id, operation_id, kind, recovery_class, status, effective_input, created_at, attempt)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7)",
            rusqlite::params![
                effect.id.as_uuid().to_string(),
                request.operation_id.as_uuid().to_string(),
                effect.kind,
                serde_json::to_string(&effect.recovery_class)
                    .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?,
                effect.effective_input.to_string(),
                now_ms(),
                effect.attempt as i64,
            ],
        )?;
        if effect.kind == "model_step" {
            let model = effect.effective_input.get("model").ok_or_else(|| {
                rusqlite::Error::InvalidParameterName("model step missing model".to_owned())
            })?;
            let capability_snapshot_id = effect
                .effective_input
                .get("capability_snapshot_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "model step missing capability snapshot id".to_owned(),
                    )
                })?;
            if capability_snapshot_id != request.checkpoint.capability_snapshot.id {
                return Err(rusqlite::Error::InvalidParameterName(
                    "context manifest capability snapshot mismatch".to_owned(),
                ));
            }
            let context_manifest_id = effect
                .effective_input
                .get("context_manifest_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "model step missing context manifest id".to_owned(),
                    )
                })?;
            let manifest_exists: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM context_manifests
                    WHERE id = ?1 AND capability_snapshot_id = ?2
                )",
                rusqlite::params![context_manifest_id, capability_snapshot_id],
                |row| row.get(0),
            )?;
            if !manifest_exists {
                return Err(rusqlite::Error::InvalidParameterName(
                    "model step context manifest was not published".to_owned(),
                ));
            }
            let model_ref = model
                .get("model_ref")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName("model step missing model_ref".to_owned())
                })?;
            let capabilities: crate::provider::ModelCapabilities =
                serde_json::from_value(model.get("capabilities").cloned().ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "model step missing provider capabilities".to_owned(),
                    )
                })?)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
            let prefix_fingerprint = effect
                .effective_input
                .get("prefix_fingerprint")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "model step missing context fingerprint".to_owned(),
                    )
                })?;
            let cache_expectation = effect
                .effective_input
                .get("cache_expectation")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName(
                        "model step missing cache expectation".to_owned(),
                    )
                })?;
            if !matches!(
                cache_expectation,
                "unsupported" | "cold_start" | "prefix_reuse_expected" | "prefix_changed"
            ) {
                return Err(rusqlite::Error::InvalidParameterName(
                    "model step has an unknown cache expectation".to_owned(),
                ));
            }
            let manifest_payload: String = tx.query_row(
                "SELECT payload FROM context_manifests WHERE id = ?1",
                [context_manifest_id],
                |row| row.get(0),
            )?;
            let manifest: crate::context::ContextManifest = serde_json::from_str(&manifest_payload)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
            if manifest.stable_prefix_fingerprint(model_ref) != prefix_fingerprint {
                return Err(rusqlite::Error::InvalidParameterName(
                    "model step context fingerprint mismatch".to_owned(),
                ));
            }
            let capabilities_payload = serde_json::to_string(&capabilities)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
            let step = effect
                .effective_input
                .get("step")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidParameterName("model step missing step".to_owned())
                })?;
            tx.execute(
                "INSERT INTO model_steps (
                    effect_id, operation_id, step, model_ref, context_window,
                    capability_snapshot_id, context_manifest_id, capabilities,
                    context_fingerprint, cache_expectation, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    effect.id.as_uuid().to_string(),
                    request.operation_id.as_uuid().to_string(),
                    step as i64,
                    model_ref,
                    model
                        .get("context_window")
                        .and_then(serde_json::Value::as_u64)
                        .map(|v| v as i64),
                    capability_snapshot_id,
                    context_manifest_id,
                    capabilities_payload,
                    prefix_fingerprint,
                    cache_expectation,
                    now_ms(),
                ],
            )?;
        }
    }
    for usage in &request.usage {
        tx.execute(
            "INSERT INTO usage (session_id, operation_id, step, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                request.session_id.as_uuid().to_string(),
                request.operation_id.as_uuid().to_string(),
                usage.step as i64,
                usage.input_tokens as i64,
                usage.output_tokens as i64,
                usage.cache_read_tokens as i64,
                usage.cache_write_tokens as i64,
                now_ms(),
            ],
        )?;
    }
    for effect_id in &request.indeterminate_effects {
        let affected = tx.execute(
            "UPDATE effects SET status = 'indeterminate', settled_at = ?2
             WHERE id = ?1 AND operation_id = ?3 AND status = 'pending'",
            rusqlite::params![
                effect_id.as_uuid().to_string(),
                now_ms(),
                request.operation_id.as_uuid().to_string(),
            ],
        )?;
        if affected != 1 {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "indeterminate settlement matched no pending effect {effect_id}"
            )));
        }
    }
    for settled in &request.settled_effects {
        let affected = tx.execute(
            "UPDATE effects SET status = 'settled', settlement = ?2, settled_at = ?3
             WHERE id = ?1 AND operation_id = ?4 AND status = 'pending'",
            rusqlite::params![
                settled.id.as_uuid().to_string(),
                settled.settlement.to_string(),
                now_ms(),
                request.operation_id.as_uuid().to_string(),
            ],
        )?;
        if affected != 1 {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "settlement matched no pending effect {}",
                settled.id
            )));
        }
    }
    for item in &request.inbox {
        insert_inbox(&tx, request.session_id, request.operation_id, item)?;
    }
    for id in &request.inbox_applied {
        tx.execute(
            "UPDATE inbox_items SET status = 'applied' WHERE id = ?1",
            rusqlite::params![id.as_uuid().to_string()],
        )?;
    }
    for effect_id in &request.assistant_frames_delete {
        tx.execute(
            "DELETE FROM assistant_frames WHERE effect_id = ?1",
            rusqlite::params![effect_id.as_uuid().to_string()],
        )?;
    }
    for effect_id in &request.tool_progress_delete {
        tx.execute(
            "DELETE FROM tool_progress WHERE effect_id = ?1",
            rusqlite::params![effect_id.as_uuid().to_string()],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn insert_capability_snapshot(
    connection: &rusqlite::Transaction<'_>,
    snapshot: &crate::context::CapabilitySnapshot,
) -> Result<(), rusqlite::Error> {
    if !snapshot.is_consistent() {
        return Err(rusqlite::Error::InvalidParameterName(
            "capability snapshot id does not match its payload".to_owned(),
        ));
    }
    let payload = serde_json::to_string(snapshot)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    connection.execute(
        "INSERT INTO capability_snapshots (id, payload) VALUES (?1, ?2)
         ON CONFLICT(id) DO NOTHING",
        rusqlite::params![snapshot.id, payload],
    )?;
    let stored: String = connection.query_row(
        "SELECT payload FROM capability_snapshots WHERE id = ?1",
        [&snapshot.id],
        |row| row.get(0),
    )?;
    if stored != payload {
        return Err(rusqlite::Error::InvalidParameterName(
            "capability snapshot hash collision or mismatched payload".to_owned(),
        ));
    }
    Ok(())
}

fn insert_context_manifest(
    connection: &rusqlite::Transaction<'_>,
    manifest: &crate::context::ContextManifest,
) -> Result<(), rusqlite::Error> {
    if !manifest.is_consistent() {
        return Err(rusqlite::Error::InvalidParameterName(
            "context manifest id does not match its payload".to_owned(),
        ));
    }
    let payload = serde_json::to_string(manifest)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    connection.execute(
        "INSERT INTO context_manifests (id, capability_snapshot_id, payload)
         VALUES (?1, ?2, ?3) ON CONFLICT(id) DO NOTHING",
        rusqlite::params![manifest.id, manifest.capability_snapshot_id, payload],
    )?;
    let stored: String = connection.query_row(
        "SELECT payload FROM context_manifests WHERE id = ?1",
        [&manifest.id],
        |row| row.get(0),
    )?;
    if stored != payload {
        return Err(rusqlite::Error::InvalidParameterName(
            "context manifest hash collision or mismatched payload".to_owned(),
        ));
    }
    Ok(())
}

fn insert_inbox(
    connection: &Connection,
    session_id: SessionId,
    operation_id: OperationId,
    item: &InboxRecord,
) -> Result<(), rusqlite::Error> {
    let status = match item.status {
        InboxStatus::Pending => "pending",
        InboxStatus::Applied => "applied",
    };
    connection.execute(
        "INSERT INTO inbox_items (id, session_id, operation_id, kind, text, status, accepted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            item.id.as_uuid().to_string(),
            session_id.as_uuid().to_string(),
            operation_id.as_uuid().to_string(),
            serde_json::to_string(&item.kind)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?,
            item.text,
            status,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn insert_checkpoint(
    connection: &Connection,
    operation_id: OperationId,
    checkpoint: &CheckpointRecord,
) -> Result<(), rusqlite::Error> {
    if checkpoint.payload.capability_snapshot_id != checkpoint.capability_snapshot.id {
        return Err(rusqlite::Error::InvalidParameterName(
            "checkpoint capability snapshot mismatch".to_owned(),
        ));
    }
    let payload = serde_json::to_string(&checkpoint.payload)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    connection.execute(
        "INSERT INTO operation_state (operation_id, state_seq, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(operation_id) DO UPDATE SET
             state_seq = excluded.state_seq,
             kind = excluded.kind,
             payload = excluded.payload,
             created_at = excluded.created_at",
        rusqlite::params![
            operation_id.as_uuid().to_string(),
            checkpoint.state_seq as i64,
            checkpoint.payload.state.kind(),
            payload,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn insert_entry(
    connection: &Connection,
    session_id: SessionId,
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
        rusqlite::params![session_id.as_uuid().to_string(), crate::session::lane::MAIN],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let parent = leaf_id
        .as_deref()
        .map(|raw| {
            crate::ids::EntryId::parse(raw).ok_or_else(|| {
                rusqlite::Error::InvalidParameterName("main lane has invalid leaf id".to_owned())
            })
        })
        .transpose()?;
    let node = crate::session::tree::Entry {
        id: crate::ids::EntryId::generate(),
        parent,
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
            crate::session::lane::MAIN,
            node.id.as_uuid().to_string(),
            config,
            now_ms(),
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(
            "main lane is missing".to_owned(),
        ));
    }
    Ok(())
}

fn load_main_lane(
    connection: &Connection,
    session_id: SessionId,
) -> Result<crate::session::lane::Lane, StoreError> {
    let (name, leaf, config): (String, Option<String>, String) = connection
        .query_row(
            "SELECT name, leaf_id, config FROM lanes WHERE session_id = ?1 AND name = ?2",
            rusqlite::params![session_id.as_uuid().to_string(), crate::session::lane::MAIN],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(StoreError::from)?;
    let leaf = leaf
        .as_deref()
        .map(|raw| {
            crate::ids::EntryId::parse(raw)
                .ok_or_else(|| StoreError::Sqlite("corrupt lane leaf id".to_owned()))
        })
        .transpose()?;
    let config = serde_json::from_str(&config)
        .map_err(|err| StoreError::Sqlite(format!("corrupt lane config: {err}")))?;
    Ok(crate::session::lane::Lane { name, leaf, config })
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
    let lane = load_main_lane(connection, session_id)?;
    if lane.name != crate::session::lane::MAIN {
        return Err(StoreError::Sqlite("main lane has invalid name".to_owned()));
    }
    let session = SessionRecord {
        id: session_id,
        cwd,
        title,
        parent_session_id: parent_session_id.and_then(|text| SessionId::parse(&text)),
        // Compatibility projection while runtime model selection migrates to
        // lane config. The session row is no longer an authority for this.
        initial_model_ref: lane.config.model_ref.clone(),
    };

    let mut statement = connection.prepare(
        "SELECT id, parent_id, seq, payload FROM entries WHERE session_id = ?1 ORDER BY seq",
    )?;
    let mut entries = Vec::new();
    let mut rows = statement.query(rusqlite::params![id])?;
    let mut previous = None;
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
        if parent != previous {
            return Err(StoreError::Sqlite(format!(
                "main lane entry {node_id} does not extend its current leaf"
            )));
        }
        let payload: String = row.get(3)?;
        let node = crate::session::tree::Entry {
            id: node_id,
            parent,
            seq,
            value: decode("entry", payload)?,
        };
        previous = Some(node.id);
        entries.push((node.seq, node.value));
    }
    if lane.leaf != previous {
        return Err(StoreError::Sqlite(
            "main lane leaf does not match the persisted branch".to_owned(),
        ));
    }
    if let Some(model_ref) = entries.iter().rev().find_map(|(_, entry)| match entry {
        SessionEntry::ModelChanged { model_ref } => Some(model_ref),
        _ => None,
    }) && model_ref != &lane.config.model_ref
    {
        return Err(StoreError::Sqlite(
            "main lane model config disagrees with the migration entry".to_owned(),
        ));
    }

    let mut statement = connection.prepare(
        "SELECT o.id, o.accepted_seq, origin.lane_name, origin.source_leaf_id,
                s.state_seq, s.payload
         FROM operations o
         JOIN operation_state s ON s.operation_id = o.id
         LEFT JOIN operation_origins origin
           ON origin.operation_id = o.id AND origin.session_id = o.session_id
         WHERE o.session_id = ?1
         ORDER BY o.accepted_seq",
    )?;
    let mut operations = Vec::new();
    let mut op_rows = statement.query(rusqlite::params![id])?;
    while let Some(row) = op_rows.next()? {
        let op_id: String = row.get(0)?;
        let accepted_seq: i64 = row.get(1)?;
        let lane_name: Option<String> = row.get(2)?;
        let source_leaf_raw: Option<String> = row.get(3)?;
        let state_seq: i64 = row.get(4)?;
        let payload: String = row.get(5)?;
        let accepted_seq = u64::try_from(accepted_seq).map_err(|_| {
            StoreError::Sqlite(format!("corrupt operation accepted seq {accepted_seq}"))
        })?;
        let lane_name = lane_name.ok_or_else(|| {
            StoreError::Sqlite(format!("operation {op_id} has no immutable origin"))
        })?;
        let source_leaf = source_leaf_raw
            .as_deref()
            .map(|raw| {
                crate::ids::EntryId::parse(raw).ok_or_else(|| {
                    StoreError::Sqlite(format!("operation {op_id} has corrupt source leaf"))
                })
            })
            .transpose()?;
        let checkpoint: CheckpointPayload = decode("checkpoint", payload)?;
        let snapshot_payload: String = connection.query_row(
            "SELECT payload FROM capability_snapshots WHERE id = ?1",
            [&checkpoint.capability_snapshot_id],
            |row| row.get(0),
        )?;
        let capability_snapshot: crate::context::CapabilitySnapshot =
            decode("capability snapshot", snapshot_payload)?;
        if capability_snapshot.id != checkpoint.capability_snapshot_id {
            return Err(StoreError::Sqlite(
                "checkpoint capability snapshot id mismatch".to_owned(),
            ));
        }
        let uuid = Uuid::parse_str(&op_id)
            .map_err(|err| StoreError::Sqlite(format!("corrupt operation id: {err}")))?;
        operations.push(LoadedOperation {
            id: OperationId::from_uuid(uuid),
            accepted_seq,
            lane_name,
            source_leaf,
            latest: (state_seq as u64, checkpoint),
            capability_snapshot,
        });
    }

    let mut statement = connection.prepare(
        "SELECT id, kind, text FROM inbox_items
         WHERE session_id = ?1 AND status = 'pending' ORDER BY accepted_at",
    )?;
    let mut pending_inbox = Vec::new();
    let mut inbox_rows = statement.query(rusqlite::params![id])?;
    while let Some(row) = inbox_rows.next()? {
        let item_id: String = row.get(0)?;
        let kind: String = row.get(1)?;
        let text: String = row.get(2)?;
        let uuid = Uuid::parse_str(&item_id)
            .map_err(|err| StoreError::Sqlite(format!("corrupt inbox id: {err}")))?;
        pending_inbox.push(InboxRecord {
            id: InboxId::from_uuid(uuid),
            kind: decode("inbox kind", kind)?,
            text,
            status: InboxStatus::Pending,
        });
    }

    let mut statement = connection.prepare(
        "SELECT effect_id, operation_id, step, frame_seq, text, thinking
         FROM assistant_frames WHERE session_id = ?1 ORDER BY frame_seq",
    )?;
    let mut assistant_frames = Vec::new();
    let mut frame_rows = statement.query(rusqlite::params![id])?;
    while let Some(row) = frame_rows.next()? {
        let effect_id: String = row.get(0)?;
        let operation_id: String = row.get(1)?;
        let effect_id = Uuid::parse_str(&effect_id)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
        let operation_id = Uuid::parse_str(&operation_id)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
        assistant_frames.push(AssistantFrame {
            effect_id: EffectId::from_uuid(effect_id),
            session_id,
            operation_id: OperationId::from_uuid(operation_id),
            step: row.get::<_, i64>(2)? as u64,
            frame_seq: row.get::<_, i64>(3)? as u64,
            text: row.get(4)?,
            thinking: row.get(5)?,
        });
    }

    let mut statement = connection.prepare(
        "SELECT effect_id, operation_id, call_id, output
         FROM tool_progress WHERE session_id = ?1 ORDER BY updated_at",
    )?;
    let mut tool_progress = Vec::new();
    let mut progress_rows = statement.query(rusqlite::params![id])?;
    while let Some(row) = progress_rows.next()? {
        let effect_id: String = row.get(0)?;
        let operation_id: String = row.get(1)?;
        let effect_id = Uuid::parse_str(&effect_id)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
        let operation_id = Uuid::parse_str(&operation_id)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
        tool_progress.push(ToolProgressCheckpoint {
            effect_id: EffectId::from_uuid(effect_id),
            session_id,
            operation_id: OperationId::from_uuid(operation_id),
            call_id: row.get::<_, i64>(2)? as u64,
            output: row.get(3)?,
        });
    }

    let latest_usage = connection
        .query_row(
            "SELECT operation_id, step, input_tokens, output_tokens,
                    cache_read_tokens, cache_write_tokens
             FROM usage WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
            [id],
            usage_row,
        )
        .optional()?;

    Ok(LoadedSession {
        session,
        entries,
        operations,
        pending_inbox,
        assistant_frames,
        tool_progress,
        latest_usage,
    })
}

fn entry_kind(entry: &SessionEntry) -> &'static str {
    match entry {
        SessionEntry::UserMessage { .. } => "user_message",
        SessionEntry::ModelChanged { .. } => "model_changed",
        SessionEntry::AssistantMessage { .. } => "assistant_message",
        SessionEntry::ToolCall { .. } => "tool_call",
        SessionEntry::ToolResult { .. } => "tool_result",
        SessionEntry::Compaction { .. } => "compaction",
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_extend_main_lane_and_config_write_does_not_move_history() {
        let mut connection = Connection::open_in_memory().expect("database");
        crate::store::schema::create_fresh(&mut connection).expect("schema");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        let session_id = SessionId::from_uuid(Uuid::nil());
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

        append_entry(
            &mut connection,
            session_id,
            &EntryRecord {
                seq: 1,
                entry: SessionEntry::UserMessage {
                    text: "hello".to_owned(),
                },
            },
        )
        .expect("first entry");
        append_entry(
            &mut connection,
            session_id,
            &EntryRecord {
                seq: 2,
                entry: SessionEntry::AssistantMessage {
                    text: "hi".to_owned(),
                },
            },
        )
        .expect("second entry");

        let first: String = connection
            .query_row("SELECT id FROM entries WHERE seq = 1", [], |row| row.get(0))
            .expect("first id");
        let second: String = connection
            .query_row("SELECT id FROM entries WHERE seq = 2", [], |row| row.get(0))
            .expect("second id");
        let first_id = crate::EntryId::parse(&first).expect("first entry id");
        let second_id = crate::EntryId::parse(&second).expect("second entry id");
        assert_ne!(first_id, second_id);
        assert_eq!(first_id.as_uuid().get_version_num(), 7);
        assert_eq!(second_id.as_uuid().get_version_num(), 7);

        let first_parent: Option<String> = connection
            .query_row("SELECT parent_id FROM entries WHERE seq = 1", [], |row| {
                row.get(0)
            })
            .expect("first parent");
        let second_parent: Option<String> = connection
            .query_row("SELECT parent_id FROM entries WHERE seq = 2", [], |row| {
                row.get(0)
            })
            .expect("second parent");
        let leaf_before: Option<String> = connection
            .query_row(
                "SELECT leaf_id FROM lanes WHERE session_id = ?1 AND name = 'main'",
                [session_id.as_uuid().to_string()],
                |row| row.get(0),
            )
            .expect("leaf before");
        assert!(first_parent.is_none());
        assert_eq!(second_parent.as_deref(), Some(first.as_str()));
        assert_eq!(leaf_before.as_deref(), Some(second.as_str()));

        set_main_lane_config(
            &mut connection,
            session_id,
            &crate::session::lane::Config::new("model-b"),
        )
        .expect("model config");

        let loaded = load(&connection, session_id).expect("load");
        let leaf_after: Option<String> = connection
            .query_row(
                "SELECT leaf_id FROM lanes WHERE session_id = ?1 AND name = 'main'",
                [session_id.as_uuid().to_string()],
                |row| row.get(0),
            )
            .expect("leaf after");
        assert_eq!(loaded.session.initial_model_ref, "model-b");
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(leaf_after, leaf_before);
    }
}
