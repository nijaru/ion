use super::support::*;

#[tokio::test]
async fn lane_agent_identity_and_lane_are_published_atomically() {
    let store = SessionStore::open_in_memory().expect("store");
    let session_id = crate::SessionId::generate();
    store
        .create_session(SessionRecord {
            id: session_id,
            cwd: "/tmp".to_owned(),
            title: String::new(),
            initial_model_ref: "model-a".to_owned(),
            control_parent_session_id: None,
            fork_source_session_id: None,
            fork_source_entry_id: None,
        })
        .await
        .expect("session");

    let root_agent = crate::AgentId::root(session_id);
    let loaded = store.load(session_id).await.expect("root load");
    assert_eq!(loaded.agents.len(), 1);
    assert_eq!(loaded.agents[0].id, root_agent);
    assert!(matches!(
        loaded.agents[0].history,
        crate::store::AgentHistory::Root
    ));

    let root = EntryRecord::provision(
        1,
        crate::SessionEntry::UserMessage {
            text: "shared root".to_owned(),
        },
    );
    let root_entry = root.id;
    store
        .append_entry(session_id, crate::session::lane::MAIN, root)
        .await
        .expect("root entry");

    let worker = crate::AgentId::generate();
    store
        .admit_lane_agent(
            session_id,
            worker,
            root_agent,
            "agent-worker",
            Some(root_entry),
            crate::session::lane::Config::new("model-a"),
        )
        .await
        .expect("agent admission");

    let loaded = store.load(session_id).await.expect("agent load");
    let agent = loaded
        .agents
        .iter()
        .find(|agent| agent.id == worker)
        .expect("worker agent");
    assert_eq!(agent.control_parent_id, Some(root_agent));
    assert_eq!(agent.session_id, session_id);
    assert_eq!(agent.lane_name, "agent-worker");
    assert!(matches!(
        agent.history,
        crate::store::AgentHistory::SharedLane {
            source_session_id,
            source_entry_id: Some(entry),
        } if source_session_id == session_id && entry == root_entry
    ));
    let lane = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == "agent-worker")
        .expect("agent lane");
    assert_eq!(lane.state.leaf, Some(root_entry));

    // The lane insert happens before the agent insert inside one transaction.
    // Reusing the immutable AgentId therefore forces the later insert to fail;
    // the preceding lane insert must roll back with it.
    assert!(
        store
            .admit_lane_agent(
                session_id,
                worker,
                root_agent,
                "must-not-survive",
                Some(root_entry),
                crate::session::lane::Config::new("model-a"),
            )
            .await
            .is_err()
    );
    let loaded = store.load(session_id).await.expect("post-failure load");
    assert_eq!(loaded.agents.len(), 2);
    assert!(
        loaded
            .lanes
            .iter()
            .all(|lane| lane.name != "must-not-survive")
    );
}

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
    assert!(matches!(
        root.agents[0].history,
        crate::store::AgentHistory::Root
    ));
    let family = store
        .load_agent_family(root_session)
        .await
        .expect("family topology");
    assert_eq!(family.len(), 3);
    assert!(family.iter().any(|agent| agent.id == root_agent));
    assert!(family.iter().any(|agent| agent.id == fresh_agent));
    assert!(family.iter().any(|agent| agent.id == fork_agent));

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
