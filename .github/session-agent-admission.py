from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"anchor missing in {path}: {old[:100]!r}")
    p.write_text(text.replace(old, new, 1))


# Schema v22: a separately hosted session can be the durable target of an
# agent in another family. Keep current child-session root rows legal during
# migration, while making fresh/fork topology explicit and immutable.
replace_once(
    "crates/ion-core/src/store/schema.rs",
    "const SCHEMA_VERSION: i64 = 21;",
    "const SCHEMA_VERSION: i64 = 22;",
)
replace_once(
    "crates/ion-core/src/store/schema.rs",
    '''        (history_kind = 'lane'\n         AND control_parent_id IS NOT NULL\n         AND source_session_id IS NOT NULL)\n''',
    '''        (history_kind = 'lane'\n         AND control_parent_id IS NOT NULL\n         AND session_id = family_session_id\n         AND source_session_id = family_session_id)\n        OR\n        (history_kind = 'fresh'\n         AND control_parent_id IS NOT NULL\n         AND id = session_id\n         AND session_id != family_session_id\n         AND lane_name = 'main'\n         AND source_session_id IS NULL\n         AND source_entry_id IS NULL)\n        OR\n        (history_kind = 'fork'\n         AND control_parent_id IS NOT NULL\n         AND id = session_id\n         AND session_id != family_session_id\n         AND lane_name = 'main'\n         AND source_session_id IS NOT NULL)\n''',
)

# Typed durable topology and store command/API.
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''pub(crate) enum AgentHistory {\n    Root,\n    SharedLane {\n        source_session_id: SessionId,\n        source_entry_id: Option<EntryId>,\n    },\n}\n''',
    '''pub(crate) enum AgentHistory {\n    Root,\n    SharedLane {\n        source_session_id: SessionId,\n        source_entry_id: Option<EntryId>,\n    },\n    Fresh,\n    Fork {\n        source_session_id: SessionId,\n        source_entry_id: Option<EntryId>,\n    },\n}\n''',
)
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    AdmitLaneAgent {\n        session_id: SessionId,\n        agent_id: AgentId,\n        control_parent_id: AgentId,\n        lane: crate::session::lane::Lane,\n        reply: oneshot::Sender<Result<(), StoreError>>,\n    },\n''',
    '''    AdmitLaneAgent {\n        session_id: SessionId,\n        agent_id: AgentId,\n        control_parent_id: AgentId,\n        lane: crate::session::lane::Lane,\n        reply: oneshot::Sender<Result<(), StoreError>>,\n    },\n    AdmitSessionAgent {\n        record: SessionRecord,\n        control_parent_id: AgentId,\n        reply: oneshot::Sender<Result<AgentId, StoreError>>,\n    },\n''',
)
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    /// Durably accept an operation: the operation row, its root inbox\n''',
    '''    /// Atomically publish a separately hosted fresh/fork session and its\n    /// family-scoped agent identity. The session, main lane, and immutable\n    /// topology either all become visible or none do.\n    pub(crate) async fn admit_session_agent(\n        &self,\n        record: SessionRecord,\n        control_parent_id: AgentId,\n    ) -> Result<AgentId, StoreError> {\n        self.request(|reply| StoreCommand::AdmitSessionAgent {\n            record,\n            control_parent_id,\n            reply,\n        })\n        .await\n    }\n\n    /// Durably accept an operation: the operation row, its root inbox\n''',
)
replace_once(
    "crates/ion-core/src/store/mod.rs",
    '''    /// Durable semantic identities retained by this session family.\n    pub(crate) agents: Vec<AgentRecord>,\n''',
    '''    /// Durable semantic identities that directly address lanes in this\n    /// session. Family-wide topology may also contain agents whose target is a\n    /// different durable session.\n    pub(crate) agents: Vec<AgentRecord>,\n''',
)

# Store thread dispatch.
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        StoreCommand::AdmitLaneAgent {\n            session_id,\n            agent_id,\n            control_parent_id,\n            lane,\n            reply,\n        } => {\n            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {\n                admit_lane_agent(connection, session_id, agent_id, control_parent_id, &lane)\n                    .map_err(StoreError::from)\n            }));\n        }\n''',
    '''        StoreCommand::AdmitLaneAgent {\n            session_id,\n            agent_id,\n            control_parent_id,\n            lane,\n            reply,\n        } => {\n            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {\n                admit_lane_agent(connection, session_id, agent_id, control_parent_id, &lane)\n                    .map_err(StoreError::from)\n            }));\n        }\n        StoreCommand::AdmitSessionAgent {\n            record,\n            control_parent_id,\n            reply,\n        } => {\n            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {\n                admit_session_agent(connection, &record, control_parent_id)\n                    .map_err(StoreError::from)\n            }));\n        }\n''',
)

# Admission-first transaction for separate-session agents. Family identity is
# derived from the exact parent agent, not from a caller-supplied family id.
p = Path("crates/ion-core/src/store/sql.rs")
text = p.read_text()
insert_at = text.index("\nfn set_lane_config(\n")
function = r'''
fn admit_session_agent(
    connection: &mut Connection,
    record: &SessionRecord,
    control_parent_id: AgentId,
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
    let (family_session, parent_session): (String, String) = tx.query_row(
        "SELECT family_session_id, session_id FROM agents WHERE id = ?1",
        [&parent],
        |row| Ok((row.get(0)?, row.get(1)?)),
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
    let config = serde_json::to_string(&crate::session::lane::Config::new(
        record.initial_model_ref.clone(),
    ))
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
'''
p.write_text(text[:insert_at] + "\n" + function + text[insert_at:])

# LoadedSession is session-local. This keeps root/lane runtime recovery stable
# when its family acquires separately hosted descendants, while allowing a
# child session provisioned admission-first to load its own fresh/fork address.
p = Path("crates/ion-core/src/store/sql.rs")
text = p.read_text()
start = text.index("fn load_agents(\n")
end = text.index("\nfn load(connection: &Connection", start)
replacement = r'''fn load_agents(
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
        matches!(agent.history, AgentHistory::Fresh | AgentHistory::Fork { .. })
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
                let parent = agent.control_parent_id.expect("lane topology checked above");
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
'''
p.write_text(text[:start] + replacement + text[end:])

# Store-level coverage: fresh/fork semantic lineage and a deliberately late
# PK collision prove the session/lane/agent publication transaction rolls back.
p = Path("crates/ion-core/src/tests/agent_store.rs")
text = p.read_text()
text += r'''

#[tokio::test]
async fn session_agents_publish_fresh_and_fork_topology_atomically() {
    let store = SessionStore::open_in_memory().expect("store");
    let root_session = crate::SessionId::generate();
    store
        .create_session(SessionRecord {
            id: root_session,
            cwd: "/tmp/root".to_owned(),
            title: String::new(),
            initial_model_ref: "model-a".to_owned(),
            control_parent_session_id: None,
            fork_source_session_id: None,
            fork_source_entry_id: None,
        })
        .await
        .expect("root session");
    let root_agent = crate::AgentId::root(root_session);
    let source = EntryRecord::provision(
        1,
        crate::SessionEntry::UserMessage {
            text: "fork boundary".to_owned(),
        },
    );
    let source_entry = source.id;
    store
        .append_entry(root_session, crate::session::lane::MAIN, source)
        .await
        .expect("source entry");

    let fresh_session = crate::SessionId::generate();
    let fresh_agent = store
        .admit_session_agent(
            SessionRecord {
                id: fresh_session,
                cwd: "/tmp/root".to_owned(),
                title: String::new(),
                initial_model_ref: "model-b".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: None,
                fork_source_entry_id: None,
            },
            root_agent,
        )
        .await
        .expect("fresh admission");
    assert_eq!(fresh_agent, crate::AgentId::root(fresh_session));
    let fresh = store.load(fresh_session).await.expect("fresh load");
    assert_eq!(fresh.agents.len(), 1);
    assert_eq!(fresh.agents[0].id, fresh_agent);
    assert_eq!(fresh.agents[0].family_session_id, root_session);
    assert_eq!(fresh.agents[0].control_parent_id, Some(root_agent));
    assert!(matches!(
        fresh.agents[0].history,
        crate::store::AgentHistory::Fresh
    ));

    let fork_session = crate::SessionId::generate();
    let fork_agent = store
        .admit_session_agent(
            SessionRecord {
                id: fork_session,
                cwd: "/tmp/root".to_owned(),
                title: String::new(),
                initial_model_ref: "model-c".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: Some(root_session),
                fork_source_entry_id: Some(source_entry),
            },
            root_agent,
        )
        .await
        .expect("fork admission");
    let fork = store.load(fork_session).await.expect("fork load");
    assert_eq!(fork.agents.len(), 1);
    assert_eq!(fork.agents[0].id, fork_agent);
    assert_eq!(fork.agents[0].family_session_id, root_session);
    assert!(matches!(
        fork.agents[0].history,
        crate::store::AgentHistory::Fork {
            source_session_id,
            source_entry_id: Some(entry),
        } if source_session_id == root_session && entry == source_entry
    ));

    // Loading the root session remains session-local even though its family now
    // owns two separately hosted descendants.
    let root = store.load(root_session).await.expect("root reload");
    assert_eq!(root.agents.len(), 1);
    assert!(matches!(root.agents[0].history, crate::store::AgentHistory::Root));

    store.close().await.expect("close store");
}

#[tokio::test]
async fn session_agent_admission_rolls_back_session_and_lane_on_late_identity_failure() {
    let store = SessionStore::open_in_memory().expect("store");
    let root_session = crate::SessionId::generate();
    store
        .create_session(SessionRecord {
            id: root_session,
            cwd: "/tmp/root".to_owned(),
            title: String::new(),
            initial_model_ref: "model-a".to_owned(),
            control_parent_session_id: None,
            fork_source_session_id: None,
            fork_source_entry_id: None,
        })
        .await
        .expect("root session");
    let root_agent = crate::AgentId::root(root_session);

    let collision = crate::AgentId::generate();
    store
        .admit_lane_agent(
            root_session,
            collision,
            root_agent,
            "collision-lane",
            None,
            crate::session::lane::Config::new("model-a"),
        )
        .await
        .expect("collision lane agent");
    let target_session = crate::SessionId::from_uuid(collision.as_uuid());
    let err = store
        .admit_session_agent(
            SessionRecord {
                id: target_session,
                cwd: "/tmp/root".to_owned(),
                title: String::new(),
                initial_model_ref: "model-a".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: None,
                fork_source_entry_id: None,
            },
            root_agent,
        )
        .await
        .expect_err("agent identity collision must reject admission");
    assert!(err.to_string().contains("UNIQUE"), "got: {err}");
    assert!(matches!(
        store.load(target_session).await,
        Err(crate::store::StoreError::NotFound(id)) if id == target_session
    ));

    store.close().await.expect("close store");
}
'''
p.write_text(text)
