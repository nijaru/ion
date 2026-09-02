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
        StoreCommand::CreateLane {
            session_id,
            lane,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                create_lane(connection, session_id, &lane).map_err(StoreError::from)
            }));
        }
        StoreCommand::AdmitLaneAgent {
            session_id,
            agent_id,
            control_parent_id,
            lane,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                admit_lane_agent(connection, session_id, agent_id, control_parent_id, &lane)
                    .map_err(StoreError::from)
            }));
        }
        StoreCommand::AdmitSessionAgent {
            record,
            control_parent_id,
            config,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                admit_session_agent(connection, &record, control_parent_id, &config)
                    .map_err(StoreError::from)
            }));
        }
        StoreCommand::BeginOperation {
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
                .map_err(StoreError::from)
            }));
        }
        StoreCommand::Commit { request, reply } => {
            let _ = reply.send(
                check_injected(fail_next_write)
                    .and_then(|()| commit(connection, &request).map_err(StoreError::from)),
            );
        }
        #[cfg(test)]
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
        StoreCommand::SetLaneConfig {
            session_id,
            lane_name,
            config,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                set_lane_config(connection, session_id, &lane_name, &config)
                    .map_err(StoreError::from)
            }));
        }
        StoreCommand::QueueNextRun {
            session_id,
            lane_name,
            next_run,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                queue_next_run(connection, session_id, &lane_name, &next_run)
                    .map_err(StoreError::from)
            }));
        }
        StoreCommand::ClearNextRun {
            session_id,
            lane_name,
            reply,
        } => {
            let _ = reply.send(
                check_injected(fail_next_write)
                    .and_then(|()| clear_next_run(connection, session_id, &lane_name)),
            );
        }
        StoreCommand::Load { session_id, reply } => {
            let _ = reply.send(load(connection, session_id));
        }
        StoreCommand::LoadAgentFamily {
            family_session_id,
            reply,
        } => {
            let _ = reply.send(load_agent_family(connection, family_session_id));
        }
        StoreCommand::LoadFamilyAgent {
            family_session_id,
            agent_id,
            reply,
        } => {
            let _ = reply.send(load_family_agent(connection, family_session_id, agent_id));
        }
        #[cfg(test)]
        StoreCommand::Usage { session_id, reply } => {
            let _ = reply.send(usage_rows(connection, session_id));
        }
        StoreCommand::LatestSession { reply } => {
            let _ = reply.send(latest_session(connection));
        }
        StoreCommand::ListSessions { reply } => {
            let _ = reply.send(list_sessions(connection));
        }
        StoreCommand::RenameSession {
            session_id,
            title,
            reply,
        } => {
            let _ = reply.send(
                check_injected(fail_next_write)
                    .and_then(|()| rename_session(connection, session_id, &title)),
            );
        }
        StoreCommand::DeleteSession { session_id, reply } => {
            let _ = reply.send(
                check_injected(fail_next_write)
                    .and_then(|()| delete_session(connection, session_id)),
            );
        }
        StoreCommand::CloneSession {
            source,
            target,
            fork_source_entry_id,
            title,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                clone_session(connection, source, target, fork_source_entry_id, &title)
            }));
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

/// One picker row: durable session identity plus presentation metadata.
fn session_summary_row(row: &rusqlite::Row<'_>) -> Result<SessionSummary, rusqlite::Error> {
    let id: String = row.get(0)?;
    let title: String = row.get(1)?;
    let updated_at: i64 = row.get(2)?;
    let entry_count: i64 = row.get(3)?;
    Ok(SessionSummary {
        id: SessionId::parse(&id).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("corrupt session id {id:?}").into(),
            )
        })?,
        title,
        updated_at: u64::try_from(updated_at).unwrap_or(0),
        entry_count: u64::try_from(entry_count).unwrap_or(0),
    })
}

fn list_sessions(connection: &Connection) -> Result<Vec<SessionSummary>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT s.id, s.title, s.updated_at, (
            SELECT COUNT(*) FROM entries e WHERE e.session_id = s.id
        )
         FROM sessions s
         WHERE s.control_parent_session_id IS NULL
         ORDER BY s.updated_at DESC, s.created_at DESC",
    )?;
    let rows = statement
        .query_map([], session_summary_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn rename_session(
    connection: &mut Connection,
    session_id: SessionId,
    title: &str,
) -> Result<(), StoreError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(StoreError::InvalidTitle);
    }
    let updated = connection
        .execute(
            "UPDATE sessions SET title = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![session_id.as_uuid().to_string(), title, now_ms()],
        )
        .map_err(StoreError::from)?;
    if updated == 0 {
        return Err(StoreError::NotFound(session_id));
    }
    Ok(())
}

/// Delete a session and all dependent rows. Hosted-agent descendants
/// (`control_parent_session_id`) are deleted with their root per family
/// integrity; the picker only ever lists roots, so this is the explicit
/// root-delete path.
fn delete_session(connection: &mut Connection, session_id: SessionId) -> Result<(), StoreError> {
    let tx = connection.transaction()?;
    // A root takes its hosted descendants with it; deleting a hosted
    // session directly would strand control lineage. Dependent rows
    // are removed children-first. operation_origins rows are
    // delete-trigger-protected; the trigger is dropped for this
    // transaction's delete and recreated immediately, so origins leave
    // with their operation without weakening the invariant elsewhere.
    let exists = {
        let mut statement = tx.prepare(
            "SELECT 1 FROM sessions WHERE id = ?1 OR control_parent_session_id = ?1 LIMIT 1",
        )?;
        statement
            .query([session_id.as_uuid().to_string()])?
            .next()?
            .is_some()
    };
    if !exists {
        tx.rollback().map_err(StoreError::from)?;
        return Err(StoreError::NotFound(session_id));
    }
    let ids = [session_id.as_uuid().to_string()];
    // Immutable-guarded rows (append-only revisions, immutable agent
    // topology, immutable operation origins) leave with their session
    // inside this one transaction: drop each guard, delete the rows,
    // recreate the guard. Nothing outside this transaction observes the
    // missing trigger, so the durable invariants they enforce are never
    // relaxed for ordinary writes.
    tx.execute(
        "DROP TRIGGER IF EXISTS operation_state_revisions_no_delete",
        [],
    )?;
    tx.execute("DROP TRIGGER IF EXISTS agents_no_delete", [])?;
    tx.execute("DROP TRIGGER IF EXISTS operation_origins_no_delete", [])?;
    for sql in [
        "DELETE FROM operation_state_revisions WHERE operation_id IN (
            SELECT id FROM operations WHERE session_id = ?1)",
        "DELETE FROM operation_state WHERE operation_id IN (
            SELECT id FROM operations WHERE session_id = ?1)",
        "DELETE FROM model_steps WHERE operation_id IN (
            SELECT id FROM operations WHERE session_id = ?1)",
        "DELETE FROM assistant_frames WHERE operation_id IN (
            SELECT id FROM operations WHERE session_id = ?1)",
        "DELETE FROM tool_progress WHERE operation_id IN (
            SELECT id FROM operations WHERE session_id = ?1)",
        "DELETE FROM inbox_items WHERE session_id = ?1",
        "DELETE FROM effects WHERE operation_id IN (
            SELECT id FROM operations WHERE session_id = ?1)",
    ] {
        tx.execute(sql, ids.clone())?;
    }
    tx.execute(
        "DELETE FROM operation_origins WHERE operation_id IN (
            SELECT id FROM operations WHERE session_id = ?1)",
        ids.clone(),
    )?;
    tx.execute("DELETE FROM operations WHERE session_id = ?1", ids.clone())?;
    tx.execute(
        "DELETE FROM agents WHERE family_session_id = ?1 OR session_id = ?1 OR source_session_id = ?1",
        ids.clone(),
    )?;
    for sql in [
        "DELETE FROM lanes WHERE session_id = ?1",
        "DELETE FROM entries WHERE session_id = ?1",
        "DELETE FROM usage WHERE session_id = ?1",
        "DELETE FROM sessions WHERE id = ?1 OR control_parent_session_id = ?1",
    ] {
        tx.execute(sql, ids.clone())?;
    }
    // Re-arm every immutable guard before commit.
    tx.execute(
        "CREATE TRIGGER IF NOT EXISTS operation_state_revisions_no_delete
         BEFORE DELETE ON operation_state_revisions
         BEGIN
             SELECT RAISE(ABORT, 'operation state revisions are append-only');
         END",
        [],
    )?;
    tx.execute(
        "CREATE TRIGGER IF NOT EXISTS agents_no_delete
         BEFORE DELETE ON agents
         BEGIN
             SELECT RAISE(ABORT, 'agent topology is immutable');
         END",
        [],
    )?;
    tx.execute(
        "CREATE TRIGGER IF NOT EXISTS operation_origins_no_delete
         BEFORE DELETE ON operation_origins
         BEGIN
             SELECT RAISE(ABORT, 'operation origin is immutable');
         END",
        [],
    )?;
    tx.commit().map_err(StoreError::from)?;
    Ok(())
}

/// Clone a session: copy the semantic conversation tree and every lane
/// into a fresh durable session with history lineage pointing back at the
/// source (DESIGN.md §13.2). The clone is a semantic snapshot at the
/// main-lane tip; durable operation/agent/usage records stay with the
/// source because they are execution history, not conversation content.
fn clone_session(
    connection: &mut Connection,
    source: SessionId,
    target: SessionId,
    fork_source_entry_id: Option<EntryId>,
    title: &str,
) -> Result<(), StoreError> {
    let loaded = load(connection, source)?;
    if loaded.session.control_parent_session_id.is_some() {
        return Err(StoreError::CloneHostedSession(source));
    }
    let tx = connection.transaction()?;
    let now = now_ms();
    // Copy the tree preserving shape: fresh entry ids (ids are globally
    // unique and belong to the source) and a seq map from old id to new
    // id so parent links and lane leaves survive the copy.
    let mut statement = tx.prepare(
        "SELECT id, parent_id, kind, payload, created_at FROM entries
         WHERE session_id = ?1 ORDER BY seq",
    )?;
    let rows = statement
        .query_map([source.as_uuid().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (id, _, _, _, _) in &rows {
        let fresh = crate::ids::EntryId::generate().as_uuid().to_string();
        id_map.insert(id.clone(), fresh);
    }
    tx.execute(
        "INSERT INTO sessions (
            id, created_at, updated_at, cwd, title, control_parent_session_id,
            fork_source_session_id, fork_source_entry_id
         ) VALUES (?1, ?2, ?2, ?3, ?4, NULL, ?5, ?6)",
        rusqlite::params![
            target.as_uuid().to_string(),
            now,
            loaded.session.cwd,
            title,
            source.as_uuid().to_string(),
            fork_source_entry_id.map(|id| id.as_uuid().to_string()),
        ],
    )
    .map_err(StoreError::from)?;
    for (seq, (id, parent, kind, payload, created_at)) in rows.into_iter().enumerate() {
        let mapped_parent = parent
            .as_ref()
            .and_then(|parent| id_map.get(parent).cloned());
        tx.execute(
            "INSERT INTO entries (
                session_id, seq, id, parent_id, kind, payload, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                target.as_uuid().to_string(),
                i64::try_from(seq).unwrap_or(i64::MAX) + 1,
                id_map.get(&id).cloned().unwrap_or_else(|| id.clone()),
                mapped_parent,
                kind,
                payload,
                created_at,
            ],
        )
        .map_err(StoreError::from)?;
    }
    // Lanes copy with remapped leaf pointers and a cleared
    // open-operation/pending pointer: a clone must not resurrect a
    // half-finished operation.
    let mut statement = tx.prepare(
        "SELECT name, leaf_id, config, created_at FROM lanes
         WHERE session_id = ?1 ORDER BY name",
    )?;
    let lane_rows = statement
        .query_map([source.as_uuid().to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (name, leaf, config, created_at) in lane_rows {
        let mapped_leaf = leaf.as_ref().and_then(|leaf| id_map.get(leaf).cloned());
        tx.execute(
            "INSERT INTO lanes (
                session_id, name, leaf_id, current_operation_id,
                pending_entry_id, pending_prompt, config, created_at, updated_at
            ) VALUES (?1, ?2, ?3, NULL, NULL, NULL, ?4, ?5, ?6)",
            rusqlite::params![
                target.as_uuid().to_string(),
                name,
                mapped_leaf,
                config,
                created_at,
                now,
            ],
        )
        .map_err(StoreError::from)?;
    }
    // The clone's own root agent row: durable family topology requires a
    // root address even though control lineage is absent (a user clone).
    tx.execute(
        "INSERT INTO agents (
            id, family_session_id, control_parent_id, session_id, lane_name,
            history_kind, source_session_id, source_entry_id, created_at
         ) VALUES (?1, ?2, NULL, ?2, 'main', 'root', NULL, NULL, ?3)",
        rusqlite::params![
            crate::ids::AgentId::root(target).as_uuid().to_string(),
            target.as_uuid().to_string(),
            now,
        ],
    )
    .map_err(StoreError::from)?;
    tx.commit().map_err(StoreError::from)?;
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

#[cfg(test)]
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
        "INSERT INTO sessions (
            id, created_at, updated_at, cwd, title, control_parent_session_id,
            fork_source_session_id, fork_source_entry_id
         ) VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            record.id.as_uuid().to_string(),
            now,
            record.cwd,
            record.title,
            record
                .control_parent_session_id
                .map(|id| id.as_uuid().to_string()),
            record
                .fork_source_session_id
                .map(|id| id.as_uuid().to_string()),
            record
                .fork_source_entry_id
                .map(|id| id.as_uuid().to_string()),
        ],
    )?;
    let config = crate::session::lane::Config::new(record.initial_model_ref.clone());
    let config = serde_json::to_string(&config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    tx.execute(
        "INSERT INTO lanes (
            session_id, name, leaf_id, current_operation_id,
            pending_entry_id, pending_prompt, config, created_at, updated_at
         ) VALUES (?1, ?2, NULL, NULL, NULL, NULL, ?3, ?4, ?4)",
        rusqlite::params![
            record.id.as_uuid().to_string(),
            crate::session::lane::MAIN,
            config,
            now,
        ],
    )?;
    let root_agent = AgentId::root(record.id).as_uuid().to_string();
    tx.execute(
        "INSERT INTO agents (
            id, family_session_id, control_parent_id, session_id, lane_name,
            history_kind, source_session_id, source_entry_id, created_at
         ) VALUES (?1, ?2, NULL, ?2, 'main', 'root', NULL, NULL, ?3)",
        rusqlite::params![root_agent, record.id.as_uuid().to_string(), now],
    )?;
    tx.commit()
}

fn create_lane(
    connection: &mut Connection,
    session_id: SessionId,
    lane: &crate::session::lane::Lane,
) -> Result<(), rusqlite::Error> {
    if lane.name.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "lane name cannot be empty".to_owned(),
        ));
    }
    if lane.state.current_operation.is_some() || lane.state.pending_next_run.is_some() {
        return Err(rusqlite::Error::InvalidParameterName(
            "a new lane must be idle and have no pending input".to_owned(),
        ));
    }
    let tx = connection.transaction()?;
    let config = serde_json::to_string(&lane.config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    let now = now_ms();
    tx.execute(
        "INSERT INTO lanes (
            session_id, name, leaf_id, current_operation_id,
            pending_entry_id, pending_prompt, config, created_at, updated_at
         ) VALUES (?1, ?2, ?3, NULL, NULL, NULL, ?4, ?5, ?5)",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            lane.name,
            lane.state.leaf.map(|id| id.as_uuid().to_string()),
            config,
            now,
        ],
    )?;
    let changed = tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![session_id.as_uuid().to_string(), now],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(
            "lane session is missing".to_owned(),
        ));
    }
    tx.commit()
}

fn admit_lane_agent(
    connection: &mut Connection,
    session_id: SessionId,
    agent_id: AgentId,
    control_parent_id: AgentId,
    lane: &crate::session::lane::Lane,
) -> Result<(), rusqlite::Error> {
    if lane.name.is_empty() || lane.name == crate::session::lane::MAIN {
        return Err(rusqlite::Error::InvalidParameterName(
            "agent lane name must be non-empty and non-main".to_owned(),
        ));
    }
    if lane.state.current_operation.is_some() || lane.state.pending_next_run.is_some() {
        return Err(rusqlite::Error::InvalidParameterName(
            "a new agent lane must be idle and have no pending input".to_owned(),
        ));
    }
    let tx = connection.transaction()?;
    let session = session_id.as_uuid().to_string();
    let parent = control_parent_id.as_uuid().to_string();
    let (parent_family, parent_session, parent_leaf, parent_config): (
        String,
        String,
        Option<String>,
        String,
    ) = tx.query_row(
        "SELECT a.family_session_id, a.session_id, lane.leaf_id, lane.config
         FROM agents a
         JOIN lanes lane ON lane.session_id = a.session_id AND lane.name = a.lane_name
         WHERE a.id = ?1",
        [&parent],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let source_leaf = lane.state.leaf.map(|id| id.as_uuid().to_string());
    if parent_family != session || parent_session != session || parent_leaf != source_leaf {
        return Err(rusqlite::Error::InvalidParameterName(
            "agent lane must anchor at its control parent's current lane leaf".to_owned(),
        ));
    }
    let parent_config: crate::session::lane::Config = serde_json::from_str(&parent_config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    if lane.config.model_ref != parent_config.model_ref
        || !lane.config.tools.is_subset_of(&parent_config.tools)
        || !lane.config.scopes.is_subset_of(&parent_config.scopes)
    {
        return Err(rusqlite::Error::InvalidParameterName(
            "agent lane configuration may narrow but not escalate its control parent".to_owned(),
        ));
    }
    let config = serde_json::to_string(&lane.config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    let now = now_ms();
    tx.execute(
        "INSERT INTO lanes (
            session_id, name, leaf_id, current_operation_id,
            pending_entry_id, pending_prompt, config, created_at, updated_at
         ) VALUES (?1, ?2, ?3, NULL, NULL, NULL, ?4, ?5, ?5)",
        rusqlite::params![session, lane.name, source_leaf, config, now],
    )?;
    tx.execute(
        "INSERT INTO agents (
            id, family_session_id, control_parent_id, session_id, lane_name,
            history_kind, source_session_id, source_entry_id, created_at
         ) VALUES (?1, ?2, ?3, ?2, ?4, 'lane', ?2, ?5, ?6)",
        rusqlite::params![
            agent_id.as_uuid().to_string(),
            session,
            parent,
            lane.name,
            source_leaf,
            now,
        ],
    )?;
    let changed = tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![session, now],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(
            "agent family session is missing".to_owned(),
        ));
    }
    tx.commit()
}

fn admit_session_agent(
    connection: &mut Connection,
    record: &SessionRecord,
    control_parent_id: AgentId,
    config: &crate::session::lane::Config,
) -> Result<AgentId, rusqlite::Error> {
    let expected_parent_session = record.control_parent_session_id.ok_or_else(|| {
        rusqlite::Error::InvalidParameterName(
            "a separately hosted agent session requires a control parent session".to_owned(),
        )
    })?;
    if record.fork_source_entry_id.is_some() && record.fork_source_session_id.is_none() {
        return Err(rusqlite::Error::InvalidParameterName(
            "a fork source entry requires a fork source session".to_owned(),
        ));
    }

    let tx = connection.transaction()?;
    let parent = control_parent_id.as_uuid().to_string();
    let (family_session, parent_session, parent_config): (String, String, String) = tx.query_row(
        "SELECT a.family_session_id, a.session_id, lane.config
             FROM agents a
             JOIN lanes lane ON lane.session_id = a.session_id AND lane.name = a.lane_name
             WHERE a.id = ?1",
        [&parent],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if parent_session != expected_parent_session.as_uuid().to_string() {
        return Err(rusqlite::Error::InvalidParameterName(
            "session control lineage does not match the exact parent agent".to_owned(),
        ));
    }
    let target_session = record.id.as_uuid().to_string();
    if target_session == family_session {
        return Err(rusqlite::Error::InvalidParameterName(
            "a fresh/fork agent must target a distinct durable session".to_owned(),
        ));
    }
    let parent_config: crate::session::lane::Config = serde_json::from_str(&parent_config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    if config.model_ref.as_str() != record.initial_model_ref.as_str() {
        return Err(rusqlite::Error::InvalidParameterName(
            "hosted agent lane model must match its admitted session model".to_owned(),
        ));
    }
    if !config.tools.is_subset_of(&parent_config.tools)
        || !config.scopes.is_subset_of(&parent_config.scopes)
    {
        return Err(rusqlite::Error::InvalidParameterName(
            "hosted agent capabilities may narrow but not escalate its control parent".to_owned(),
        ));
    }

    let now = now_ms();
    tx.execute(
        "INSERT INTO sessions (
            id, created_at, updated_at, cwd, title, control_parent_session_id,
            fork_source_session_id, fork_source_entry_id
         ) VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            target_session,
            now,
            record.cwd,
            record.title,
            expected_parent_session.as_uuid().to_string(),
            record
                .fork_source_session_id
                .map(|id| id.as_uuid().to_string()),
            record
                .fork_source_entry_id
                .map(|id| id.as_uuid().to_string()),
        ],
    )?;
    let config = serde_json::to_string(config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    tx.execute(
        "INSERT INTO lanes (
            session_id, name, leaf_id, current_operation_id,
            pending_entry_id, pending_prompt, config, created_at, updated_at
         ) VALUES (?1, 'main', NULL, NULL, NULL, NULL, ?2, ?3, ?3)",
        rusqlite::params![target_session, config, now],
    )?;

    let agent_id = AgentId::root(record.id);
    let history_kind = if record.fork_source_session_id.is_some() {
        "fork"
    } else {
        "fresh"
    };
    tx.execute(
        "INSERT INTO agents (
            id, family_session_id, control_parent_id, session_id, lane_name,
            history_kind, source_session_id, source_entry_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, 'main', ?5, ?6, ?7, ?8)",
        rusqlite::params![
            agent_id.as_uuid().to_string(),
            family_session,
            parent,
            target_session,
            history_kind,
            record
                .fork_source_session_id
                .map(|id| id.as_uuid().to_string()),
            record
                .fork_source_entry_id
                .map(|id| id.as_uuid().to_string()),
            now,
        ],
    )?;
    tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![family_session, now],
    )?;
    tx.commit()?;
    Ok(agent_id)
}

fn set_lane_config(
    connection: &mut Connection,
    session_id: SessionId,
    lane_name: &str,
    config: &crate::session::lane::Config,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let config = serde_json::to_string(config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    let now = now_ms();
    let changed = tx.execute(
        "UPDATE lanes SET config = ?3, updated_at = ?4
         WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![session_id.as_uuid().to_string(), lane_name, config, now,],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "lane {lane_name:?} is missing"
        )));
    }
    tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![session_id.as_uuid().to_string(), now],
    )?;
    tx.commit()
}

fn queue_next_run(
    connection: &mut Connection,
    session_id: SessionId,
    lane_name: &str,
    next_run: &crate::session::lane::NextRun,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let entry_id = next_run.entry_id.as_uuid().to_string();
    let entry_exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM entries WHERE id = ?1)",
        [&entry_id],
        |row| row.get(0),
    )?;
    if entry_exists {
        return Err(rusqlite::Error::InvalidParameterName(
            "reserved next-run entry identity already exists".to_owned(),
        ));
    }
    let now = now_ms();
    let changed = tx.execute(
        "UPDATE lanes SET
            pending_entry_id = ?3,
            pending_prompt = ?4,
            updated_at = ?5
         WHERE session_id = ?1 AND name = ?2
           AND current_operation_id IS NOT NULL
           AND pending_entry_id IS NULL",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            lane_name,
            entry_id,
            next_run.prompt,
            now,
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "lane {lane_name:?} cannot reserve another next run"
        )));
    }
    tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![session_id.as_uuid().to_string(), now],
    )?;
    tx.commit()
}

/// Remove one lane's pending next-run input durably. Returns the cleared
/// prompt so a frontend can restore it (pi parity: alt+up dequeue).
fn clear_next_run(
    connection: &mut Connection,
    session_id: SessionId,
    lane_name: &str,
) -> Result<Option<String>, StoreError> {
    let tx = connection.transaction()?;
    let cleared: Option<String> = tx
        .query_row(
            "SELECT pending_prompt FROM lanes
             WHERE session_id = ?1 AND name = ?2 AND pending_entry_id IS NOT NULL",
            rusqlite::params![session_id.as_uuid().to_string(), lane_name],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    if cleared.is_some() {
        tx.execute(
            "UPDATE lanes SET
                pending_entry_id = NULL,
                pending_prompt = NULL,
                updated_at = ?3
             WHERE session_id = ?1 AND name = ?2 AND pending_entry_id IS NOT NULL",
            rusqlite::params![session_id.as_uuid().to_string(), lane_name, now_ms(),],
        )?;
    }
    tx.commit().map_err(StoreError::from)?;
    Ok(cleared)
}

#[cfg(test)]
fn append_entry(
    connection: &mut Connection,
    session_id: SessionId,
    lane_name: &str,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    insert_entry(&tx, session_id, lane_name, entry)?;
    tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![session_id.as_uuid().to_string(), now_ms()],
    )?;
    tx.commit()
}

fn begin_operation(
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
    let accepted_text = match (&root_inbox.kind, &entry.entry) {
        (InboxKind::Prompt, SessionEntry::UserMessage { text }) => text,
        (
            InboxKind::AgentMessage { from },
            SessionEntry::AgentMessage {
                from: entry_from,
                text,
            },
        ) if from == entry_from => text,
        _ => {
            return Err(rusqlite::Error::InvalidParameterName(
                "operation root inbox disagrees with its semantic entry".to_owned(),
            ));
        }
    };
    if accepted_text != &checkpoint.payload.prompt || root_inbox.text.as_str() != accepted_text {
        return Err(rusqlite::Error::InvalidParameterName(
            "operation input disagrees with its accepted entry".to_owned(),
        ));
    }
    match (pending_entry, pending_prompt) {
        (None, None) => {}
        (Some(reserved_entry), Some(reserved_prompt))
            if matches!(&root_inbox.kind, InboxKind::Prompt)
                && reserved_entry == entry_id
                && reserved_prompt.as_str() == accepted_text => {}
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

fn commit(connection: &mut Connection, request: &CommitRequest) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let operation_id = request.operation_id.as_uuid().to_string();
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
    }
    insert_capability_snapshot(&tx, &request.checkpoint.capability_snapshot)?;
    for manifest in &request.context_manifests {
        insert_context_manifest(&tx, manifest)?;
    }
    insert_checkpoint(&tx, request.operation_id, &request.checkpoint)?;
    for entry in &request.entries {
        insert_entry(&tx, request.session_id, &lane_name, entry)?;
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
    if matches!(
        &request.checkpoint.payload.state,
        OperationState::Finished(_)
    ) {
        let changed = tx.execute(
            "UPDATE lanes SET current_operation_id = NULL, updated_at = ?4
             WHERE session_id = ?1 AND name = ?2 AND current_operation_id = ?3",
            rusqlite::params![
                request.session_id.as_uuid().to_string(),
                lane_name,
                operation_id,
                now_ms(),
            ],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidParameterName(
                "terminal operation no longer owns its immutable origin lane".to_owned(),
            ));
        }
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
    if let InboxKind::AgentMessage { from } = &item.kind {
        let sender_exists: bool = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM agents WHERE id = ?1 AND family_session_id = ?2
            )",
            rusqlite::params![from.as_uuid().to_string(), session_id.as_uuid().to_string(),],
            |row| row.get(0),
        )?;
        if !sender_exists {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "agent message sender {from} is not retained by this family"
            )));
        }
    }
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

    let leaf_id: Option<String> = connection.query_row(
        "SELECT leaf_id FROM lanes WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![session_id.as_uuid().to_string(), lane_name],
        |row| row.get(0),
    )?;
    let expected_parent = leaf_id
        .as_deref()
        .map(|raw| {
            crate::ids::EntryId::parse(raw).ok_or_else(|| {
                rusqlite::Error::InvalidParameterName(format!(
                    "lane {lane_name:?} has invalid leaf id"
                ))
            })
        })
        .transpose()?;
    if entry.parent != expected_parent {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "entry parent does not match lane {lane_name:?}'s leaf"
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

    let changed = connection.execute(
        "UPDATE lanes SET leaf_id = ?3, updated_at = ?4
         WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            lane_name,
            node.id.as_uuid().to_string(),
            now_ms(),
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "lane {lane_name:?} is missing"
        )));
    }
    Ok(())
}

fn load_lanes(
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
                    StoreError::Sqlite(format!("lane {lane_name:?} has a corrupt pending entry id"))
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

fn load_family_agent(
    connection: &Connection,
    family_session_id: SessionId,
    agent_id: AgentId,
) -> Result<Option<AgentRecord>, StoreError> {
    let raw_session: Option<String> = connection
        .query_row(
            "SELECT session_id FROM agents WHERE id = ?1 AND family_session_id = ?2",
            rusqlite::params![
                agent_id.as_uuid().to_string(),
                family_session_id.as_uuid().to_string(),
            ],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw_session) = raw_session else {
        return Ok(None);
    };
    let session_id = SessionId::parse(&raw_session).ok_or_else(|| {
        StoreError::Sqlite(format!("agent {agent_id} has a corrupt target session id"))
    })?;
    let loaded = load(connection, session_id)?;
    let agent = loaded
        .agents
        .into_iter()
        .find(|agent| agent.id == agent_id && agent.family_session_id == family_session_id)
        .ok_or_else(|| {
            StoreError::Sqlite(format!(
                "agent {agent_id} is missing from its addressed session"
            ))
        })?;
    Ok(Some(agent))
}

fn load_agent_family(
    connection: &Connection,
    family_session_id: SessionId,
) -> Result<Vec<AgentRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT session_id FROM agents
         WHERE family_session_id = ?1 ORDER BY session_id",
    )?;
    let mut rows = statement.query([family_session_id.as_uuid().to_string()])?;
    let mut session_ids = Vec::new();
    while let Some(row) = rows.next()? {
        let raw: String = row.get(0)?;
        let session_id = SessionId::parse(&raw).ok_or_else(|| {
            StoreError::Sqlite("agent family has a corrupt target session id".to_owned())
        })?;
        session_ids.push(session_id);
    }
    drop(rows);
    drop(statement);

    let mut agents = Vec::new();
    for session_id in session_ids {
        let loaded = load(connection, session_id)?;
        agents.extend(
            loaded
                .agents
                .into_iter()
                .filter(|agent| agent.family_session_id == family_session_id),
        );
    }
    let root = AgentId::root(family_session_id);
    if !agents.iter().any(|agent| {
        agent.id == root
            && agent.session_id == family_session_id
            && matches!(agent.history, AgentHistory::Root)
    }) {
        return Err(StoreError::Sqlite(format!(
            "agent family {family_session_id} has no durable root"
        )));
    }
    Ok(agents)
}

fn load_agents(
    connection: &Connection,
    session_id: SessionId,
    lane_names: &std::collections::HashSet<String>,
    entry_ids: &std::collections::HashSet<EntryId>,
) -> Result<Vec<AgentRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, family_session_id, control_parent_id, session_id, lane_name,
                history_kind, source_session_id, source_entry_id
         FROM agents WHERE session_id = ?1 ORDER BY created_at, id",
    )?;
    let mut rows = statement.query([session_id.as_uuid().to_string()])?;
    let mut agents = Vec::new();
    while let Some(row) = rows.next()? {
        let raw_id: String = row.get(0)?;
        let raw_family: String = row.get(1)?;
        let raw_parent: Option<String> = row.get(2)?;
        let raw_session: String = row.get(3)?;
        let lane_name: String = row.get(4)?;
        let history_kind: String = row.get(5)?;
        let raw_source_session: Option<String> = row.get(6)?;
        let raw_source_entry: Option<String> = row.get(7)?;
        let id = AgentId::parse(&raw_id)
            .ok_or_else(|| StoreError::Sqlite("corrupt agent id".to_owned()))?;
        let family_session_id = SessionId::parse(&raw_family)
            .ok_or_else(|| StoreError::Sqlite(format!("agent {id} has corrupt family id")))?;
        let control_parent_id = raw_parent
            .as_deref()
            .map(|raw| {
                AgentId::parse(raw).ok_or_else(|| {
                    StoreError::Sqlite(format!("agent {id} has corrupt control parent"))
                })
            })
            .transpose()?;
        let target_session_id = SessionId::parse(&raw_session)
            .ok_or_else(|| StoreError::Sqlite(format!("agent {id} has corrupt target session")))?;
        if target_session_id != session_id {
            return Err(StoreError::Sqlite(format!(
                "agent {id} does not address the loaded session"
            )));
        }
        if !lane_names.contains(&lane_name) {
            return Err(StoreError::Sqlite(format!(
                "agent {id} addresses missing lane {lane_name:?}"
            )));
        }
        let history = match history_kind.as_str() {
            "root" => {
                if family_session_id != session_id
                    || id != AgentId::root(session_id)
                    || control_parent_id.is_some()
                    || lane_name != crate::session::lane::MAIN
                    || raw_source_session.is_some()
                    || raw_source_entry.is_some()
                {
                    return Err(StoreError::Sqlite(format!(
                        "root agent {id} has inconsistent topology"
                    )));
                }
                AgentHistory::Root
            }
            "lane" => {
                let source_session_id = raw_source_session
                    .as_deref()
                    .and_then(SessionId::parse)
                    .ok_or_else(|| {
                        StoreError::Sqlite(format!("agent {id} has corrupt history session"))
                    })?;
                if family_session_id != session_id
                    || source_session_id != session_id
                    || control_parent_id.is_none()
                {
                    return Err(StoreError::Sqlite(format!(
                        "lane agent {id} has inconsistent family topology"
                    )));
                }
                let source_entry_id = raw_source_entry
                    .as_deref()
                    .map(|raw| {
                        EntryId::parse(raw).ok_or_else(|| {
                            StoreError::Sqlite(format!("agent {id} has corrupt history entry"))
                        })
                    })
                    .transpose()?;
                if source_entry_id.is_some_and(|entry| !entry_ids.contains(&entry)) {
                    return Err(StoreError::Sqlite(format!(
                        "agent {id} names a missing history entry"
                    )));
                }
                AgentHistory::SharedLane {
                    source_session_id,
                    source_entry_id,
                }
            }
            "fresh" => {
                if family_session_id == session_id
                    || id != AgentId::root(session_id)
                    || control_parent_id.is_none()
                    || lane_name != crate::session::lane::MAIN
                    || raw_source_session.is_some()
                    || raw_source_entry.is_some()
                {
                    return Err(StoreError::Sqlite(format!(
                        "fresh agent {id} has inconsistent topology"
                    )));
                }
                AgentHistory::Fresh
            }
            "fork" => {
                let source_session_id = raw_source_session
                    .as_deref()
                    .and_then(SessionId::parse)
                    .ok_or_else(|| {
                        StoreError::Sqlite(format!("agent {id} has corrupt fork source session"))
                    })?;
                let source_entry_id = raw_source_entry
                    .as_deref()
                    .map(|raw| {
                        EntryId::parse(raw).ok_or_else(|| {
                            StoreError::Sqlite(format!("agent {id} has corrupt fork source entry"))
                        })
                    })
                    .transpose()?;
                if family_session_id == session_id
                    || id != AgentId::root(session_id)
                    || control_parent_id.is_none()
                    || lane_name != crate::session::lane::MAIN
                {
                    return Err(StoreError::Sqlite(format!(
                        "fork agent {id} has inconsistent topology"
                    )));
                }
                AgentHistory::Fork {
                    source_session_id,
                    source_entry_id,
                }
            }
            other => {
                return Err(StoreError::Sqlite(format!(
                    "agent {id} has unknown history kind {other:?}"
                )));
            }
        };
        agents.push(AgentRecord {
            id,
            family_session_id,
            control_parent_id,
            session_id: target_session_id,
            lane_name,
            history,
        });
    }

    let ids = agents
        .iter()
        .map(|agent| agent.id)
        .collect::<std::collections::HashSet<_>>();
    let has_local_root = ids.contains(&AgentId::root(session_id))
        && agents
            .iter()
            .any(|agent| matches!(agent.history, AgentHistory::Root));
    let hosted_agent = agents.iter().find(|agent| {
        matches!(
            agent.history,
            AgentHistory::Fresh | AgentHistory::Fork { .. }
        )
    });
    if !has_local_root && hosted_agent.is_none() {
        return Err(StoreError::Sqlite(
            "session has no durable agent address".to_owned(),
        ));
    }

    for agent in &agents {
        match &agent.history {
            AgentHistory::Root => {}
            AgentHistory::SharedLane { .. } => {
                let parent = agent
                    .control_parent_id
                    .expect("lane topology checked above");
                if !ids.contains(&parent) {
                    return Err(StoreError::Sqlite(format!(
                        "agent {} names a missing local control parent {parent}",
                        agent.id
                    )));
                }
            }
            AgentHistory::Fresh | AgentHistory::Fork { .. } => {
                let parent = agent
                    .control_parent_id
                    .expect("hosted topology checked above")
                    .as_uuid()
                    .to_string();
                let (parent_family, parent_session): (String, String) = connection
                    .query_row(
                        "SELECT family_session_id, session_id FROM agents WHERE id = ?1",
                        [&parent],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(StoreError::from)?;
                if parent_family != agent.family_session_id.as_uuid().to_string() {
                    return Err(StoreError::Sqlite(format!(
                        "agent {} crosses family control lineage",
                        agent.id
                    )));
                }
                let (raw_control_session, raw_fork_session, raw_fork_entry): (
                    Option<String>,
                    Option<String>,
                    Option<String>,
                ) = connection.query_row(
                    "SELECT control_parent_session_id, fork_source_session_id,
                            fork_source_entry_id
                     FROM sessions WHERE id = ?1",
                    [session_id.as_uuid().to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                if raw_control_session.as_deref() != Some(parent_session.as_str()) {
                    return Err(StoreError::Sqlite(format!(
                        "agent {} control lineage disagrees with its session",
                        agent.id
                    )));
                }
                match &agent.history {
                    AgentHistory::Fresh => {
                        if raw_fork_session.is_some() || raw_fork_entry.is_some() {
                            return Err(StoreError::Sqlite(format!(
                                "fresh agent {} has fork session lineage",
                                agent.id
                            )));
                        }
                    }
                    AgentHistory::Fork {
                        source_session_id,
                        source_entry_id,
                    } => {
                        if raw_fork_session.as_deref()
                            != Some(source_session_id.as_uuid().to_string().as_str())
                            || raw_fork_entry.as_deref()
                                != source_entry_id
                                    .map(|entry| entry.as_uuid().to_string())
                                    .as_deref()
                        {
                            return Err(StoreError::Sqlite(format!(
                                "fork agent {} history disagrees with its session",
                                agent.id
                            )));
                        }
                    }
                    AgentHistory::Root | AgentHistory::SharedLane { .. } => unreachable!(),
                }
            }
        }
    }
    Ok(agents)
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
    let (cwd, title, raw_control_parent, raw_fork_session, raw_fork_entry): (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = connection
        .query_row(
            "SELECT cwd, title, control_parent_session_id, fork_source_session_id,
                    fork_source_entry_id
             FROM sessions WHERE id = ?1",
            rusqlite::params![id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound(session_id),
            other => StoreError::from(other),
        })?;
    let parse_session_lineage = |raw: Option<String>, label: &str| {
        raw.map(|value| {
            SessionId::parse(&value)
                .ok_or_else(|| StoreError::Sqlite(format!("session has corrupt {label} id")))
        })
        .transpose()
    };
    let control_parent_session_id = parse_session_lineage(raw_control_parent, "control parent")?;
    let fork_source_session_id = parse_session_lineage(raw_fork_session, "fork source session")?;
    let fork_source_entry_id = raw_fork_entry
        .map(|value| {
            EntryId::parse(&value).ok_or_else(|| {
                StoreError::Sqlite("session has corrupt fork source entry id".to_owned())
            })
        })
        .transpose()?;
    if fork_source_entry_id.is_some() && fork_source_session_id.is_none() {
        return Err(StoreError::Sqlite(
            "session fork source entry has no source session".to_owned(),
        ));
    }
    let lanes = load_lanes(connection, session_id)?;
    let main_lane = lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .ok_or_else(|| StoreError::Sqlite("session has no main lane".to_owned()))?;
    let session = SessionRecord {
        id: session_id,
        cwd,
        title,
        control_parent_session_id,
        fork_source_session_id,
        fork_source_entry_id,
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
        if lane
            .state
            .leaf
            .is_some_and(|leaf| !entry_ids.contains(&leaf))
        {
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
    let agents = load_agents(connection, session_id, &lane_names, &entry_ids)?;

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
        if !lane_names.contains(&lane_name) {
            return Err(StoreError::Sqlite(format!(
                "operation {op_id} names missing origin lane {lane_name:?}"
            )));
        }
        let source_leaf = source_leaf_raw
            .as_deref()
            .map(|raw| {
                crate::ids::EntryId::parse(raw).ok_or_else(|| {
                    StoreError::Sqlite(format!("operation {op_id} has corrupt source leaf"))
                })
            })
            .transpose()?;
        if source_leaf.is_some_and(|leaf| !entry_ids.contains(&leaf)) {
            return Err(StoreError::Sqlite(format!(
                "operation {op_id} names a missing source leaf"
            )));
        }
        let checkpoint: CheckpointPayload = decode("checkpoint", payload)?;
        let snapshot_payload: String = connection.query_row(
            "SELECT payload FROM capability_snapshots WHERE id = ?1",
            [&checkpoint.capability_snapshot_id],
            |row| row.get(0),
        )?;
        let capability_snapshot: crate::context::CapabilitySnapshot =
            decode("capability snapshot", snapshot_payload)?;
        if capability_snapshot.id != checkpoint.capability_snapshot_id
            || !capability_snapshot.is_consistent()
        {
            return Err(StoreError::Sqlite(
                "checkpoint capability snapshot is inconsistent".to_owned(),
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
            pending_inbox: Vec::new(),
        });
    }

    let mut open_by_lane = std::collections::HashMap::new();
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
        agents,
        lanes,
        entries,
        operations,
        assistant_frames,
        tool_progress,
        latest_usage,
    })
}

fn entry_kind(entry: &SessionEntry) -> &'static str {
    match entry {
        SessionEntry::UserMessage { .. } => "user_message",
        SessionEntry::AgentMessage { .. } => "agent_message",
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
                control_parent_session_id: None,
                fork_source_session_id: None,
                fork_source_entry_id: None,
            },
        )
        .expect("session");

        let first_record = EntryRecord::provision(
            1,
            SessionEntry::UserMessage {
                text: "hello".to_owned(),
            },
        );
        let first_expected = first_record.id;
        append_entry(
            &mut connection,
            session_id,
            crate::session::lane::MAIN,
            &first_record,
        )
        .expect("first entry");

        let second_record = EntryRecord::provision(
            2,
            SessionEntry::AssistantMessage {
                text: "hi".to_owned(),
            },
        )
        .after(Some(first_expected));
        let second_expected = second_record.id;
        append_entry(
            &mut connection,
            session_id,
            crate::session::lane::MAIN,
            &second_record,
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
        assert_eq!(first_id, first_expected);
        assert_eq!(second_id, second_expected);
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

        set_lane_config(
            &mut connection,
            session_id,
            crate::session::lane::MAIN,
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

    #[test]
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
                control_parent_session_id: None,
                fork_source_session_id: None,
                fork_source_entry_id: None,
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

        create_lane(
            &mut connection,
            session_id,
            &crate::session::lane::Lane {
                name: "worker".to_owned(),
                state: crate::session::lane::State {
                    leaf: Some(root_id),
                    current_operation: None,
                    pending_next_run: None,
                },
                config: crate::session::lane::Config::new("model-b"),
            },
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
        append_entry(&mut connection, session_id, "worker", &worker_child).expect("worker child");

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
                control_parent_session_id: None,
                fork_source_session_id: None,
                fork_source_entry_id: None,
            },
        )
        .expect("session");
        let config =
            serde_json::to_string(&crate::session::lane::Config::new("model-b")).expect("config");
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
        assert_eq!(
            worker_leaf.as_deref(),
            Some(entry_id.as_uuid().to_string().as_str())
        );
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
                control_parent_session_id: None,
                fork_source_session_id: None,
                fork_source_entry_id: None,
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
