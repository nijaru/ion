from pathlib import Path


def replace_one(path: str, old: str, new: str, label: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    file.write_text(text.replace(old, new, 1))


def insert_before(path: str, anchor: str, addition: str, label: str) -> None:
    replace_one(path, anchor, addition + anchor, label)


# Store command/API carries the exact initial lane configuration.
replace_one(
    "crates/ion-core/src/store/mod.rs",
    '''    AdmitSessionAgent {
        record: SessionRecord,
        control_parent_id: AgentId,
        reply: oneshot::Sender<Result<AgentId, StoreError>>,
    },
''',
    '''    AdmitSessionAgent {
        record: SessionRecord,
        control_parent_id: AgentId,
        config: crate::session::lane::Config,
        reply: oneshot::Sender<Result<AgentId, StoreError>>,
    },
''',
    "session-agent store command",
)
replace_one(
    "crates/ion-core/src/store/mod.rs",
    '''    pub(crate) async fn admit_session_agent(
        &self,
        record: SessionRecord,
        control_parent_id: AgentId,
    ) -> Result<AgentId, StoreError> {
        self.request(|reply| StoreCommand::AdmitSessionAgent {
            record,
            control_parent_id,
            reply,
        })
        .await
    }
''',
    '''    pub(crate) async fn admit_session_agent(
        &self,
        record: SessionRecord,
        control_parent_id: AgentId,
        config: crate::session::lane::Config,
    ) -> Result<AgentId, StoreError> {
        self.request(|reply| StoreCommand::AdmitSessionAgent {
            record,
            control_parent_id,
            config,
            reply,
        })
        .await
    }
''',
    "session-agent store API",
)
replace_one(
    "crates/ion-core/src/store/sql.rs",
    '''        StoreCommand::AdmitSessionAgent {
            record,
            control_parent_id,
            reply,
        } => {
            let _ = reply.send(check_injected(fail_next_write).and_then(|()| {
                admit_session_agent(connection, &record, control_parent_id)
                    .map_err(StoreError::from)
            }));
        }
''',
    '''        StoreCommand::AdmitSessionAgent {
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
''',
    "session-agent dispatch",
)
replace_one(
    "crates/ion-core/src/store/sql.rs",
    '''fn admit_session_agent(
    connection: &mut Connection,
    record: &SessionRecord,
    control_parent_id: AgentId,
) -> Result<AgentId, rusqlite::Error> {
''',
    '''fn admit_session_agent(
    connection: &mut Connection,
    record: &SessionRecord,
    control_parent_id: AgentId,
    config: &crate::session::lane::Config,
) -> Result<AgentId, rusqlite::Error> {
''',
    "session-agent SQL signature",
)
replace_one(
    "crates/ion-core/src/store/sql.rs",
    '''    let parent = control_parent_id.as_uuid().to_string();
    let (family_session, parent_session): (String, String) = tx.query_row(
        "SELECT family_session_id, session_id FROM agents WHERE id = ?1",
        [&parent],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
''',
    '''    let parent = control_parent_id.as_uuid().to_string();
    let (family_session, parent_session, parent_config): (String, String, String) = tx
        .query_row(
            "SELECT a.family_session_id, a.session_id, lane.config
             FROM agents a
             JOIN lanes lane ON lane.session_id = a.session_id AND lane.name = a.lane_name
             WHERE a.id = ?1",
            [&parent],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
''',
    "load hosted control-parent config",
)
replace_one(
    "crates/ion-core/src/store/sql.rs",
    '''    let target_session = record.id.as_uuid().to_string();
    if target_session == family_session {
        return Err(rusqlite::Error::InvalidParameterName(
            "a fresh/fork agent must target a distinct durable session".to_owned(),
        ));
    }

    let now = now_ms();
''',
    '''    let target_session = record.id.as_uuid().to_string();
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
    if !config.tools.is_subset_of(&parent_config.tools) {
        return Err(rusqlite::Error::InvalidParameterName(
            "hosted agent capabilities may narrow but not escalate its control parent".to_owned(),
        ));
    }

    let now = now_ms();
''',
    "hosted capability validation",
)
replace_one(
    "crates/ion-core/src/store/sql.rs",
    '''    let config = serde_json::to_string(&crate::session::lane::Config::new(
        record.initial_model_ref.clone(),
    ))
    .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
''',
    '''    let config = serde_json::to_string(config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
''',
    "persist admitted hosted config",
)

# Hosted residency derives initial tools from the durable root lane, then applies
# the hosted read-only ceiling. The SQL transaction independently checks the
# subset against the current parent config to fence a concurrent narrowing.
insert_before(
    "crates/ion-core/src/agent_host.rs",
    '''    async fn spawn(
        self: &Arc<Self>,
''',
    '''    async fn initial_lane_config(
        &self,
        model_ref: &str,
    ) -> Result<crate::session::lane::Config, String> {
        let loaded = self
            .config
            .store
            .load(self.parent_id)
            .await
            .map_err(|err| format!("agent family configuration unavailable: {err}"))?;
        let root = AgentId::root(self.parent_id);
        let parent = loaded
            .agents
            .iter()
            .find(|agent| agent.id == root)
            .ok_or_else(|| "agent family root is missing".to_owned())?;
        let lane = loaded
            .lanes
            .iter()
            .find(|lane| lane.name == parent.lane_name)
            .ok_or_else(|| "agent family root lane is missing".to_owned())?;
        let mut config = lane.config.clone();
        config.model_ref = model_ref.to_owned();
        config.tools = config
            .tools
            .narrowed_by(&crate::tool::ToolSelection::read_only());
        Ok(config)
    }

''',
    "hosted initial lane config",
)
replace_one(
    "crates/ion-core/src/agent_host.rs",
    '''        let agent_id = crate::ids::AgentId::root(session_id);
        let initial_model_ref = provider.initial_model_ref();
        self.config
''',
    '''        let agent_id = crate::ids::AgentId::root(session_id);
        let initial_model_ref = provider.initial_model_ref();
        let lane_config = self.initial_lane_config(&initial_model_ref).await?;
        self.config
''',
    "derive hosted config before admission",
)
replace_one(
    "crates/ion-core/src/agent_host.rs",
    '''                },
                crate::ids::AgentId::root(self.parent_id),
            )
''',
    '''                },
                crate::ids::AgentId::root(self.parent_id),
                lane_config,
            )
''',
    "publish hosted config",
)

# Store tests pass explicit configs and cover store-side escalation rejection.
insert_before(
    "crates/ion-core/src/tests/agent_store.rs",
    '''#[tokio::test]
async fn lane_agent_identity_and_lane_are_published_atomically() {
''',
    '''fn read_only_config(model_ref: &str) -> crate::session::lane::Config {
    let mut config = crate::session::lane::Config::new(model_ref);
    config.tools = crate::tool::ToolSelection::read_only();
    config
}

''',
    "agent store config helper",
)
replace_one(
    "crates/ion-core/src/tests/agent_store.rs",
    '''                initial_model_ref: "model-b".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: None,
                fork_source_entry_id: None,
            },
            root_agent,
        )
''',
    '''                initial_model_ref: "model-b".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: None,
                fork_source_entry_id: None,
            },
            root_agent,
            read_only_config("model-b"),
        )
''',
    "fresh store admission config",
)
replace_one(
    "crates/ion-core/src/tests/agent_store.rs",
    '''                initial_model_ref: "model-c".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: Some(root_session),
                fork_source_entry_id: Some(source_entry),
            },
            root_agent,
        )
''',
    '''                initial_model_ref: "model-c".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: Some(root_session),
                fork_source_entry_id: Some(source_entry),
            },
            root_agent,
            read_only_config("model-c"),
        )
''',
    "fork store admission config",
)
replace_one(
    "crates/ion-core/src/tests/agent_store.rs",
    '''                initial_model_ref: "model-a".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: None,
                fork_source_entry_id: None,
            },
            root_agent,
        )
        .await
        .expect_err("agent identity collision must reject admission");
''',
    '''                initial_model_ref: "model-a".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: None,
                fork_source_entry_id: None,
            },
            root_agent,
            crate::session::lane::Config::new("model-a"),
        )
        .await
        .expect_err("agent identity collision must reject admission");
''',
    "collision store admission config",
)
insert_before(
    "crates/ion-core/src/tests/agent_store.rs",
    '''#[tokio::test]
async fn session_agent_admission_rolls_back_session_and_lane_on_late_identity_failure() {
''',
    '''#[tokio::test]
async fn session_agent_admission_rejects_capability_escalation() {
    let store = SessionStore::open_in_memory().expect("store");
    let root_session = crate::SessionId::generate();
    store
        .create_session(SessionRecord {
            id: root_session,
            cwd: "/tmp/root".to_owned(),
            title: String::new(),
            initial_model_ref: "parent-model".to_owned(),
            control_parent_session_id: None,
            fork_source_session_id: None,
            fork_source_entry_id: None,
        })
        .await
        .expect("root session");
    let root_agent = crate::AgentId::root(root_session);
    let mut parent_config = crate::session::lane::Config::new("parent-model");
    parent_config.tools = crate::tool::ToolSelection::Only(std::collections::BTreeSet::from([
        "read".to_owned(),
    ]));
    store
        .set_lane_config(root_session, crate::session::lane::MAIN, parent_config.clone())
        .await
        .expect("narrow root tools");

    let rejected_session = crate::SessionId::generate();
    let err = store
        .admit_session_agent(
            SessionRecord {
                id: rejected_session,
                cwd: "/tmp/root".to_owned(),
                title: String::new(),
                initial_model_ref: "child-model".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: None,
                fork_source_entry_id: None,
            },
            root_agent,
            read_only_config("child-model"),
        )
        .await
        .expect_err("hosted admission must not widen parent tools");
    assert!(
        err.to_string().contains("may narrow but not escalate"),
        "got: {err}"
    );
    assert!(matches!(
        store.load(rejected_session).await,
        Err(crate::store::StoreError::NotFound(id)) if id == rejected_session
    ));

    let admitted_session = crate::SessionId::generate();
    let mut child_config = crate::session::lane::Config::new("child-model");
    child_config.tools = parent_config.tools.clone();
    store
        .admit_session_agent(
            SessionRecord {
                id: admitted_session,
                cwd: "/tmp/root".to_owned(),
                title: String::new(),
                initial_model_ref: "child-model".to_owned(),
                control_parent_session_id: Some(root_session),
                fork_source_session_id: None,
                fork_source_entry_id: None,
            },
            root_agent,
            child_config.clone(),
        )
        .await
        .expect("narrow hosted admission");
    let loaded = store.load(admitted_session).await.expect("hosted load");
    let main = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("hosted main lane");
    assert_eq!(main.config, child_config);
    assert_eq!(loaded.session.initial_model_ref, "child-model");

    store.close().await.expect("close store");
}

''',
    "session-agent escalation coverage",
)

# Unified-host coverage asserts durable config and executor scope agree.
replace_one(
    "crates/ion-core/src/tests/hosted_agent_invariants.rs",
    '''    assert_eq!(loaded.session.cwd, workspace_text);
    assert!(loaded.entries.iter().any(|record| {
''',
    '''    assert_eq!(loaded.session.cwd, workspace_text);
    let main = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("hosted main lane");
    assert_eq!(main.config.tools, crate::tool::ToolSelection::read_only());
    assert!(loaded.entries.iter().any(|record| {
''',
    "hosted durable capability assertion",
)

# Record the concrete boundary in the current checkpoint; implementation order
# remains normative and unchanged.
replace_one(
    "DESIGN.md",
    '''Lane/fresh/fork agents share one model-facing namespace and durable family authority; a hosted-runtime service owns only fresh/fork provider/runtime/catalog residency, with no parallel child/delegate execution architecture. The current client snapshot still projects `main`; scoped capabilities are the active boundary.
''',
    '''Lane/fresh/fork agents share one model-facing namespace and durable family authority; a hosted-runtime service owns only fresh/fork provider/runtime/catalog residency, with no parallel child/delegate execution architecture. Shared-history and separately hosted admission both publish durable lane capability selections that may narrow but never exceed the control parent, and recovery re-applies that stored selection to the available executor catalog. The current client snapshot still projects `main`; scoped capability lifecycle beyond this admission boundary remains active work.
''',
    "design implementation checkpoint",
)
